#[macro_export]
macro_rules! match_provider_response {
    ($response: ident, $reply: ident, $target: pat, $target_body: block) => {
        match $response {
            $target => $target_body,
            ProviderResponse::Error(e, code) => {
                error!("received ProviderResponse::Error: ({}) {}", code, e);
                $reply.error(code);
                return;
            }
            _ => {
                error!("Received unexpected ProviderResponse: {:?}", $response);
                $reply.error(libc::EIO);
                return;
            }
        };
    };
}

#[macro_export]
macro_rules! receive_response {
    ($rx: ident, $response: ident, $reply: ident) => {
        tracing::trace!("receiving response");
        // let $response = run_async_blocking($rx.recv());

        let sync_code = std::thread::spawn(move || $rx.blocking_recv());
        let $response = sync_code.join().unwrap();
        tracing::trace!("received response");
        // $rx.close();
        // tracing::info!("closed receiver");

        reply_error_o!(
            $response,
            $reply,
            libc::EIO,
            "Failed to receive ProviderResponse",
        );
    };
}

#[macro_export]
macro_rules! send_request {
    ($tx: expr, $data:ident, $reply: ident) => {
        tracing::trace!("sending request");
        {
            let sender = $tx.clone();
            let send_res = std::thread::spawn(move || sender.blocking_send($data));
            let send_res = send_res.join().unwrap();
            reply_error_e_consuming!(
                send_res,
                $reply,
                libc::EIO,
                "Failed to send ProviderRequest",
            );
        }
        tracing::trace!("sent request");
    };
}
