use async_trait::async_trait;
use futures::future;
use hyper::{self, Body, Request, Response, StatusCode};
use log::error;
use serde::de::DeserializeOwned;
use serde_json;

use crate::util;

#[async_trait]
pub trait MyService {
    async fn handle(&self, req: Request<Body>) -> Response<Body>;
}

#[async_trait]
pub trait Handler {
    async fn handle(&self, req: Request<Body>) -> Response<Body>;

    fn respond_with(&self, status: StatusCode, msg: &str) -> Response<Body> {
        util::new_msg_resp(status, msg.to_string())
    }

    fn respond_error(&self, err: &str) -> Response<Body> {
        error!("InternalServerError: {}", err);
        util::new_empty_resp(StatusCode::INTERNAL_SERVER_ERROR)
    }
}

pub trait Filter {
    fn filter(&self, req: &Request<Body>) -> FilterResult;
}

pub enum FilterResult {
    Halt(Response<Body>),
    Continue,
}

pub struct FilteredHandler {
    filter: Box<dyn Filter>,
    handler: Box<dyn Handler>,
}

pub struct NotFoundHandler;

impl FilteredHandler {
    pub fn new(filter: Box<dyn Filter>, handler: Box<dyn Handler>) -> Box<FilteredHandler> {
        Box::new(FilteredHandler {
            filter: filter,
            handler: handler,
        })
    }
}

impl Handler for FilteredHandler {
    fn handle(&self, req: Request<Body>) -> Response<Body> {
        match self.filter.filter(&req) {
            FilterResult::Halt(resp) => Box::new(future::ok(resp)),
            FilterResult::Continue => self.handler.handle(req),
        }
    }
}

impl Handler for NotFoundHandler {
    fn handle(&self, _: Request<Body>) -> Response<Body> {
        Box::new(future::ok(util::new_empty_resp(StatusCode::NOT_FOUND)))
    }
}

pub fn parse_json<T: DeserializeOwned, F>(req: Request<Body>, func: F) -> Response<Body>
where
    F: FnOnce(T) -> Response<Body> + Send + 'static,
{
    Box::new(req.into_body().concat2().map(move |data| {
        let obj: T = match serde_json::from_slice(&data) {
            Ok(l) => l,
            Err(e) => {
                return util::new_bad_req_resp(format!("Failed to parse JSON: {}", e));
            }
        };

        func(obj)
    }))
}
