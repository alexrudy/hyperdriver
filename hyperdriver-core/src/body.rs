/// Extension trait to help clone a request that contains a `Body`.
pub trait TryCloneRequest {
    /// Try to clone the request. If the body can't be cloned, `None` is returned.
    fn try_clone_request(&self) -> Option<Self>
    where
        Self: Sized;
}

pub trait TryCloneBody {
    fn try_clone(&self) -> Option<Self>
    where
        Self: Sized;
}

impl<B> TryCloneRequest for http::Request<B>
where
    B: TryCloneBody + http_body::Body,
{
    fn try_clone_request(&self) -> Option<Self> {
        if let Some(body) = self.body().try_clone() {
            let mut req = http::Request::builder()
                .uri(self.uri().clone())
                .method(self.method().clone())
                .version(self.version());

            if let Some(headers) = req.headers_mut() {
                *headers = self.headers().clone();
            };

            Some(req.body(body).expect("request builder should succeed"))
        } else {
            None
        }
    }
}
