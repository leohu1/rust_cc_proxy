use bytes::Bytes;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::{Stream, StreamExt};

use crate::monitor::TokenMonitor;

/// Creates an SSE byte stream from a reqwest response, suitable for
/// actix-web's `HttpResponse::streaming()`.
pub fn into_sse_stream(
    response: reqwest::Response,
) -> impl Stream<Item = Result<Bytes, actix_web::Error>> {
    do_stream(response, None)
}

/// Like `into_sse_stream`, but intercepts SSE events to extract and
/// record token usage in the provided monitor.
pub fn into_sse_stream_with_monitor(
    response: reqwest::Response,
    monitor: Arc<TokenMonitor>,
) -> impl Stream<Item = Result<Bytes, actix_web::Error>> {
    do_stream(response, Some(monitor))
}

fn do_stream(
    response: reqwest::Response,
    monitor: Option<Arc<TokenMonitor>>,
) -> impl Stream<Item = Result<Bytes, actix_web::Error>> {
    // Use reqwest::Error in the channel (it is Send); convert to
    // actix_web::Error in the output stream (actix_web::Error is not Send).
    let (tx, rx) = mpsc::channel::<Result<Bytes, reqwest::Error>>(32);

    tokio::spawn(async move {
        let mut response = response;
        let mut buf = String::new();
        loop {
            match response.chunk().await {
                Ok(Some(chunk)) => {
                    // Scan for token usage in SSE data lines
                    if let Some(ref monitor) = monitor {
                        scan_chunk_for_tokens(&chunk, &mut buf, monitor);
                    }

                    if tx.send(Ok(chunk)).await.is_err() {
                        break; // client disconnected
                    }
                }
                Ok(None) => break, // stream ended
                Err(e) => {
                    let _ = tx.send(Err(e)).await;
                    break;
                }
            }
        }
    });

    ReceiverStream::new(rx)
        .map(|result| result.map_err(|e| actix_web::error::ErrorBadGateway(e.to_string())))
}

/// Scan a chunk of SSE bytes for `message_start` (input_tokens) and
/// `message_delta` (output_tokens) events.
fn scan_chunk_for_tokens(chunk: &[u8], buf: &mut String, monitor: &TokenMonitor) {
    buf.push_str(&String::from_utf8_lossy(chunk));

    while let Some(pos) = buf.find('\n') {
        let line = buf[..pos].trim().to_string();
        let remainder = buf[pos + 1..].to_string();
        *buf = remainder;

        if let Some(data) = line.strip_prefix("data: ") {
            if let Some(tokens) = TokenMonitor::parse_sse_message_start(data) {
                if tokens > 0 {
                    tracing::debug!("SSE message_start: input_tokens={tokens}");
                    monitor.record_streaming_start(tokens);
                }
            }
            if let Some(tokens) = TokenMonitor::parse_sse_message_delta(data) {
                if tokens > 0 {
                    tracing::debug!("SSE message_delta: output_tokens={tokens}");
                    monitor.record_streaming_output(tokens);
                }
            }
        }
    }
}
