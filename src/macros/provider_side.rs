#[macro_export]
macro_rules! send_error_response {
    ($request:ident, $e:expr, $code:expr,) => {
        send_error_response!($request, $e, $code)
    };
    ($request:ident, $e:expr, $code:expr) => {{
        let error_send_response = $request
            .response_sender
            .send(ProviderResponse::Error($e, $code))
            .await;
        if let Err(e) = error_send_response {
            error!("Failed to send error response: {:?}", e);
            return Err(anyhow!("Failed to send error response: {:?}", e));
        }
        Ok(())
    }};
}
#[macro_export]
macro_rules! send_response {
    ($request:ident, $response:expr,) => {
        send_response!($request, $response)
    };

    ($request:ident, $response:expr) => {{
        tracing::trace!("sending response");
        let result_send_response = $request.response_sender.send($response).await;
        if let Err(e) = result_send_response {
            error!("Failed to send result response: {:?}", e);
            return Err(anyhow!("Failed to send result response: {:?}", e));
        }
        tracing::trace!("sent response");
        Ok(())
    }};
}
