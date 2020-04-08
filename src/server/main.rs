use std::fs;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;

use log::{error, info, warn};
use hyper::{Request, Body};
use hyper::server::Server;
use hyper::service::service_fn;
use hyper::service::make_service_fn;
use hyper::server::conn::AddrStream;
use tokio;
use tokio_tls;
use native_tls::{self, Identity};

use crate::config::Config;
use crate::github;
use crate::jira;
use crate::jira::api::JiraSession;
use crate::runtime;
use crate::server::github_handler::GithubHandlerState;
use crate::server::octobot_service::OctobotService;
use crate::server::redirect_service::RedirectService;
use crate::server::sessions::Sessions;
use crate::server::http::MyService;

pub fn start(config: Config) {
    let num_http_threads = std::cmp::max(2, config.main.num_http_threads.unwrap_or(20));

    let rt = runtime::new(num_http_threads, "runtime");
    rt.block_on(async move {
        run_server(config)
    });
}

async fn run_server(config: Config) {
    let config = Arc::new(config);


    let github: Arc<dyn github::api::GithubSessionFactory>;

    if config.github.app_id.is_some() {
        github = match github::api::GithubApp::new(
            &config.github.host,
            config.github.app_id.expect("expected an app_id"),
            &config.github.app_key().expect("expected an app_key"),
        ) {
            Ok(s) => Arc::new(s),
            Err(e) => panic!("Error initiating github session: {}", e),
        };
    } else {
        github = match github::api::GithubOauthApp::new(
            &config.github.host,
            &config.github.api_token.as_ref().expect("expected an api_token"),
        ) {
            Ok(s) => Arc::new(s),
            Err(e) => panic!("Error initiating github session: {}", e),
        };
    }

    let jira: Option<Arc<dyn jira::api::Session>>;
    if let Some(ref jira_config) = config.jira {
        jira = match JiraSession::new(&jira_config) {
            Ok(s) => Some(Arc::new(s)),
            Err(e) => panic!("Error initiating jira session: {}", e),
        };
    } else {
        jira = None;
    }

    let http_addr: SocketAddr = match config.main.listen_addr {
        Some(ref addr_and_port) => addr_and_port.parse().unwrap(),
        None => "0.0.0.0:3000".parse().unwrap(),
    };

    let https_addr: SocketAddr = match config.main.listen_addr_ssl {
        Some(ref addr_and_port) => addr_and_port.parse().unwrap(),
        None => "0.0.0.0:3001".parse().unwrap(),
    };

    let tls_acceptor;
    if let Some(ref pkcs12_file) = config.main.ssl_pkcs12_file {
       let identity = load_identity(pkcs12_file, &config.main.ssl_pkcs12_pass.unwrap_or(String::new()));
       tls_acceptor = Some(tokio_tls::TlsAcceptor::from(native_tls::TlsAcceptor::builder(identity).build()?));
    } else {
        warn!("Warning: No SSL configured");
        tls_acceptor = None;
    }

    let ui_sessions = Arc::new(Sessions::new());
    let github_handler_state = Arc::new(GithubHandlerState::new(config.clone(), github.clone(), jira.clone()));

    let main_service_impl = OctobotService::new(config.clone(), ui_sessions.clone(), github_handler_state.clone());
    let redirect_service_impl = RedirectService::new(https_addr.port());

    let main_service = make_service_fn(|_: &AddrStream| {
        let service = main_service_impl.clone();
        async move {
            Ok::<_, hyper::Error>(service_fn(move |req: Request<hyper::Body>| async move {
                Ok::<_, hyper::Error>(
                    service.handle(req)
                )
            }))
        }
    });

    let redirect_service = make_service_fn(|_: &AddrStream| {
        let service = redirect_service_impl.clone();
        async move {
            Ok::<_, hyper::Error>(service_fn(move |req: Request<hyper::Body>| async move {
                Ok::<_, hyper::Error>(
                    service.handle(req)
                )
            }))
        }
    });

    if let Some(tls_acceptor) = tls_acceptor {
        // setup main service on https
        {
            let tcp = tokio::net::TcpListener::bind(&https_addr).await.unwrap();
            let tls = tcp.incoming()
                .for_each(move |tcp| {
                    let tls_accept = tls_acceptor.accept()
                        .then(|r| match r {
                            Ok(x) => Ok::<_, io::Error>(Some(x)),
                            Err(e) => {
                                error!("tls error: {}", e);
                                Ok::<_, io::Error>(None)
                            }
                        })
                        .filter_map(|x| x);
                    tokio::spawn(tls_accept);
                    Ok(())
                })
                .map_err(|err| {
                    error!("server error {:?}", err);
                });

            let server = Server::builder(tls).serve(main_service).map_err(|e| error!("server error: {}", e));
            info!("Listening (HTTPS) on {}", https_addr);
            tokio::spawn(server);
        }
        // setup http redirect
        {
            let server = Server::bind(&http_addr).serve(redirect_service).map_err(
                |e| error!("server error: {}", e),
            );
            info!("Listening (HTTP Redirect) on {}", http_addr);
            tokio::spawn(server);
        }
    } else {
        // setup main service on http
        {
            let server = Server::bind(&http_addr).serve(main_service).map(|_| ()).map_err(
                |e| error!("server error: {}", e),
            );
            info!("Listening (HTTP) on {}", http_addr);
            tokio::spawn(server);
        }
    }
}

fn load_identity(filename: &str, pass: &str) -> native_tls::Identity {
    let mut bytes = vec![];

    let file = fs::File::open(filename).expect("cannot open pkcs12 identity file");
    file.read_to_end(&mut bytes).unwrap();

    Identity::from_pkcs12(&bytes, pass).expect("cannot read pkcs12 identity")
}


