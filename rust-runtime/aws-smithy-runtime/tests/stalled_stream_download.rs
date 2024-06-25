/*
 * Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

#![cfg(all(feature = "client", feature = "test-util"))]

use std::time::Duration;

#[macro_use]
mod stalled_stream_common;
use stalled_stream_common::*;

/// Scenario: Successfully download at a rate above the minimum throughput.
/// Expected: MUST NOT timeout.
#[tokio::test]
async fn download_success() {
    let _logs = show_test_logs();

    let (time, sleep) = tick_advance_time_and_sleep();
    let (server, response_sender) = channel_server();
    let op = operation(server, time.clone(), sleep);

    let server = tokio::spawn(async move {
        for _ in 1..100 {
            response_sender.send(NEAT_DATA).await.unwrap();
            tick!(time, Duration::from_secs(1));
        }
        drop(response_sender);
        tick!(time, Duration::from_secs(1));
    });

    let response_body = op.invoke(()).await.expect("initial success");
    let result = eagerly_consume(response_body).await;
    server.await.unwrap();

    result.expect("response MUST NOT timeout");
}

/// Scenario: Download takes a some time to start, but then goes normally.
/// Expected: MUT NOT timeout.
#[tokio::test]
async fn download_slow_start() {
    let _logs = show_test_logs();

    let (time, sleep) = tick_advance_time_and_sleep();
    let (server, response_sender) = channel_server();
    let op = operation(server, time.clone(), sleep);

    let server = tokio::spawn(async move {
        // Delay almost to the end of the grace period before sending anything
        tick!(time, Duration::from_secs(4));
        for _ in 1..100 {
            response_sender.send(NEAT_DATA).await.unwrap();
            tick!(time, Duration::from_secs(1));
        }
        drop(response_sender);
        tick!(time, Duration::from_secs(1));
    });

    let response_body = op.invoke(()).await.expect("initial success");
    let result = eagerly_consume(response_body).await;
    server.await.unwrap();

    result.expect("response MUST NOT timeout");
}

/// Scenario: Download starts fine, and then slowly falls below minimum throughput.
/// Expected: MUST timeout.
#[tokio::test]
async fn download_too_slow() {
    let _logs = show_test_logs();

    let (time, sleep) = tick_advance_time_and_sleep();
    let (server, response_sender) = channel_server();
    let op = operation(server, time.clone(), sleep);

    let server = tokio::spawn(async move {
        // Get slower with every poll
        for delay in 1..100 {
            let _ = response_sender.send(NEAT_DATA).await;
            tick!(time, Duration::from_secs(delay));
        }
        drop(response_sender);
        tick!(time, Duration::from_secs(1));
    });

    let response_body = op.invoke(()).await.expect("initial success");
    let result = eagerly_consume(response_body).await;
    server.await.unwrap();

    let err = result.expect_err("should have timed out");
    assert_str_contains!(
        DisplayErrorContext(err.as_ref()).to_string(),
        "minimum throughput was specified at 1 B/s, but throughput of 0 B/s was observed"
    );
}

/// Scenario: Download starts fine, and then the server stalls and stops sending data.
/// Expected: MUST timeout.
#[tokio::test]
async fn download_stalls() {
    let _logs = show_test_logs();

    let (time, sleep) = tick_advance_time_and_sleep();
    let (server, response_sender) = channel_server();
    let op = operation(server, time.clone(), sleep);
    let barrier = Arc::new(Barrier::new(2));

    let c = barrier.clone();
    let server = tokio::spawn(async move {
        c.wait().await;
        for i in 1..10 {
            tracing::debug!("send {i}");
            response_sender.send(NEAT_DATA).await.unwrap();
            tick!(time, Duration::from_secs(1));
        }
        tick!(time, Duration::from_secs(10));
    });

    let response_body = op.invoke(()).await.expect("initial success");
    let result = tokio::spawn(async move {
        barrier.wait().await;
        eagerly_consume(response_body).await
    });
    server.await.unwrap();

    let err = result
        .await
        .expect("no panics")
        .expect_err("should have timed out");
    assert_str_contains!(
        DisplayErrorContext(err.as_ref()).to_string(),
        "minimum throughput was specified at 1 B/s, but throughput of 0 B/s was observed"
    );
}

/// Scenario: Download starts fine, but then the server stalls for a time within the
///           grace period. Following that, it starts sending data again.
/// Expected: MUST NOT timeout.
#[tokio::test]
async fn download_stall_recovery_in_grace_period() {
    let _logs = show_test_logs();

    let (time, sleep) = tick_advance_time_and_sleep();
    let (server, response_sender) = channel_server();
    let op = operation(server, time.clone(), sleep);

    let server = tokio::spawn(async move {
        for _ in 1..10 {
            response_sender.send(NEAT_DATA).await.unwrap();
            tick!(time, Duration::from_secs(1));
        }
        // Delay almost to the end of the grace period
        tick!(time, Duration::from_secs(4));
        // And now recover
        for _ in 1..10 {
            response_sender.send(NEAT_DATA).await.unwrap();
            tick!(time, Duration::from_secs(1));
        }
        drop(response_sender);
        tick!(time, Duration::from_secs(1));
    });

    let response_body = op.invoke(()).await.expect("initial success");
    let result = eagerly_consume(response_body).await;
    server.await.unwrap();

    result.expect("response MUST NOT timeout");
}

/// Scenario: The server sends data fast enough, but the customer doesn't consume the
///           data fast enough.
/// Expected: MUST NOT timeout.
#[tokio::test]
async fn user_downloads_data_too_slowly() {
    let _logs = show_test_logs();

    let (time, sleep) = tick_advance_time_and_sleep();
    let (server, response_sender) = channel_server();
    let op = operation(server, time.clone(), sleep);

    let server = tokio::spawn(async move {
        for _ in 1..100 {
            response_sender.send(NEAT_DATA).await.unwrap();
        }
        drop(response_sender);
    });

    let response_body = op.invoke(()).await.expect("initial success");
    let result = slowly_consume(time, response_body).await;
    server.await.unwrap();

    result.expect("response MUST NOT timeout");
}

use download_test_tools::*;
use tokio::sync::Barrier;
mod download_test_tools {
    use crate::stalled_stream_common::*;

    fn response(body: SdkBody) -> HttpResponse {
        HttpResponse::try_from(
            http_02x::Response::builder()
                .status(200)
                .body(body)
                .unwrap(),
        )
        .unwrap()
    }

    pub fn operation(
        http_connector: impl HttpConnector + 'static,
        time: TickAdvanceTime,
        sleep: TickAdvanceSleep,
    ) -> Operation<(), SdkBody, Infallible> {
        #[derive(Debug)]
        struct Deserializer;
        impl DeserializeResponse for Deserializer {
            fn deserialize_streaming(
                &self,
                response: &mut HttpResponse,
            ) -> Option<Result<Output, OrchestratorError<Error>>> {
                let mut body = SdkBody::taken();
                mem::swap(response.body_mut(), &mut body);
                Some(Ok(Output::erase(body)))
            }

            fn deserialize_nonstreaming(
                &self,
                _: &HttpResponse,
            ) -> Result<Output, OrchestratorError<Error>> {
                unreachable!()
            }
        }

        Operation::builder()
            .service_name("test")
            .operation_name("test")
            .http_client(FakeServer(http_connector.into_shared()))
            .endpoint_url("http://localhost:1234/doesntmatter")
            .no_auth()
            .no_retry()
            .timeout_config(TimeoutConfig::disabled())
            .serializer(|_body: ()| Ok(HttpRequest::new(SdkBody::empty())))
            .deserializer_impl(Deserializer)
            .stalled_stream_protection(
                StalledStreamProtectionConfig::enabled()
                    .grace_period(Duration::from_secs(5))
                    .build(),
            )
            .interceptor(StalledStreamProtectionInterceptor::default())
            .sleep_impl(sleep)
            .time_source(time)
            .build()
    }

    /// Fake server/connector that responds with a channel body.
    pub fn channel_server() -> (SharedHttpConnector, tokio::sync::mpsc::Sender<Bytes>) {
        #[derive(Debug)]
        struct FakeServerConnector {
            body: Arc<Mutex<Option<SdkBody>>>,
        }
        impl HttpConnector for FakeServerConnector {
            fn call(&self, _request: HttpRequest) -> HttpConnectorFuture {
                let body = self.body.lock().unwrap().take().unwrap();
                HttpConnectorFuture::new(async move { Ok(response(body)) })
            }
        }

        let (body, body_sender) = channel_body();
        (
            FakeServerConnector {
                body: Arc::new(Mutex::new(Some(body))),
            }
            .into_shared(),
            body_sender,
        )
    }

    /// Simulate a client eagerly consuming all the data sent to it from the server.
    pub async fn eagerly_consume(body: SdkBody) -> Result<(), BoxError> {
        pin_mut!(body);
        while let Some(result) = poll_fn(|cx| body.as_mut().poll_data(cx)).await {
            if let Err(err) = result {
                return Err(err);
            } else {
                info!("consumed bytes from the response body");
            }
        }
        Ok(())
    }

    /// Simulate a client very slowly consuming data with an eager server.
    ///
    /// This implementation will take longer than the grace period to consume
    /// the next piece of data.
    pub async fn slowly_consume(time: TickAdvanceTime, body: SdkBody) -> Result<(), BoxError> {
        pin_mut!(body);
        while let Some(result) = poll_fn(|cx| body.as_mut().poll_data(cx)).await {
            if let Err(err) = result {
                return Err(err);
            } else {
                info!("consumed bytes from the response body");
                tick!(time, Duration::from_secs(10));
            }
        }
        Ok(())
    }
}
