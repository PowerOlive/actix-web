//! For middleware documentation, see [`Compress`].

use std::{
    cmp,
    future::Future,
    marker::PhantomData,
    pin::Pin,
    str::FromStr,
    task::{Context, Poll},
};

use actix_http::{
    body::MessageBody,
    encoding::Encoder,
    http::header::{ContentEncoding, ACCEPT_ENCODING},
    Error,
};
use actix_service::{Service, Transform};
use actix_utils::future::{ok, Ready};
use futures_core::ready;
use pin_project::pin_project;

use crate::{
    dev::BodyEncoding,
    service::{ServiceRequest, ServiceResponse},
};

/// Middleware for compressing response payloads.
///
/// Use `BodyEncoding` trait for overriding response compression. To disable compression set
/// encoding to `ContentEncoding::Identity`.
///
/// # Examples
/// ```
/// use actix_web::{web, middleware, App, HttpResponse};
///
/// let app = App::new()
///     .wrap(middleware::Compress::default())
///     .default_service(web::to(|| HttpResponse::NotFound()));
/// ```
#[derive(Debug, Clone)]
pub struct Compress(ContentEncoding);

impl Compress {
    /// Create new `Compress` middleware with the specified encoding.
    pub fn new(encoding: ContentEncoding) -> Self {
        Compress(encoding)
    }
}

impl Default for Compress {
    fn default() -> Self {
        Compress::new(ContentEncoding::Auto)
    }
}

impl<S, B> Transform<S, ServiceRequest> for Compress
where
    B: MessageBody,
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error>,
{
    type Response = ServiceResponse<Encoder<B>>;
    type Error = Error;
    type Transform = CompressMiddleware<S>;
    type InitError = ();
    type Future = Ready<Result<Self::Transform, Self::InitError>>;

    fn new_transform(&self, service: S) -> Self::Future {
        ok(CompressMiddleware {
            service,
            encoding: self.0,
        })
    }
}

pub struct CompressMiddleware<S> {
    service: S,
    encoding: ContentEncoding,
}

impl<S, B> Service<ServiceRequest> for CompressMiddleware<S>
where
    B: MessageBody,
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error>,
{
    type Response = ServiceResponse<Encoder<B>>;
    type Error = Error;
    type Future = CompressResponse<S, B>;

    actix_service::forward_ready!(service);

    #[allow(clippy::borrow_interior_mutable_const)]
    fn call(&self, req: ServiceRequest) -> Self::Future {
        // negotiate content-encoding
        let encoding = if let Some(val) = req.headers().get(&ACCEPT_ENCODING) {
            if let Ok(enc) = val.to_str() {
                AcceptEncoding::parse(enc, self.encoding)
            } else {
                ContentEncoding::Identity
            }
        } else {
            ContentEncoding::Identity
        };

        CompressResponse {
            encoding,
            fut: self.service.call(req),
            _phantom: PhantomData,
        }
    }
}

#[pin_project]
pub struct CompressResponse<S, B>
where
    S: Service<ServiceRequest>,
    B: MessageBody,
{
    #[pin]
    fut: S::Future,
    encoding: ContentEncoding,
    _phantom: PhantomData<B>,
}

impl<S, B> Future for CompressResponse<S, B>
where
    B: MessageBody,
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error>,
{
    type Output = Result<ServiceResponse<Encoder<B>>, Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();

        match ready!(this.fut.poll(cx)) {
            Ok(resp) => {
                let enc = if let Some(enc) = resp.response().get_encoding() {
                    enc
                } else {
                    *this.encoding
                };

                Poll::Ready(Ok(
                    resp.map_body(move |head, body| Encoder::response(enc, head, body))
                ))
            }
            Err(e) => Poll::Ready(Err(e)),
        }
    }
}

struct AcceptEncoding {
    encoding: ContentEncoding,
    quality: f64,
}

impl Eq for AcceptEncoding {}

impl Ord for AcceptEncoding {
    #[allow(clippy::comparison_chain)]
    fn cmp(&self, other: &AcceptEncoding) -> cmp::Ordering {
        if self.quality > other.quality {
            cmp::Ordering::Less
        } else if self.quality < other.quality {
            cmp::Ordering::Greater
        } else {
            cmp::Ordering::Equal
        }
    }
}

impl PartialOrd for AcceptEncoding {
    fn partial_cmp(&self, other: &AcceptEncoding) -> Option<cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for AcceptEncoding {
    fn eq(&self, other: &AcceptEncoding) -> bool {
        self.quality == other.quality
    }
}

impl AcceptEncoding {
    fn new(tag: &str) -> Option<AcceptEncoding> {
        let parts: Vec<&str> = tag.split(';').collect();
        let encoding = match parts.len() {
            0 => return None,
            _ => ContentEncoding::from(parts[0]),
        };
        let quality = match parts.len() {
            1 => encoding.quality(),
            _ => f64::from_str(parts[1]).unwrap_or(0.0),
        };
        Some(AcceptEncoding { encoding, quality })
    }

    /// Parse a raw Accept-Encoding header value into an ordered list.
    pub fn parse(raw: &str, encoding: ContentEncoding) -> ContentEncoding {
        let mut encodings = raw
            .replace(' ', "")
            .split(',')
            .map(|l| AcceptEncoding::new(l))
            .flatten()
            .collect::<Vec<_>>();

        encodings.sort();

        for enc in encodings {
            if encoding == ContentEncoding::Auto {
                return enc.encoding;
            } else if encoding == enc.encoding {
                return encoding;
            }
        }

        ContentEncoding::Identity
    }
}
