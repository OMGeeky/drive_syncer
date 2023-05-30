// this macro will generate the following code if called with the following arguments:
// call:
// let response: Option<x> = ...;
// reply_error_o!(response, response_out, libc::EIO, "Failed to receive ProviderResponse");
// result:
// if response.is_none() {
//             error!("Failed to receive ProviderResponse");
//             reply.error(libc::EIO);
//             return;
// }
// let response_out = response.unwrap();

#[macro_export]
macro_rules! reply_error_o_consuming {
    ($option_in:ident, $reply:ident, $error_code:expr, $error_msg:expr) => {
        reply_error_o!($option_in, _unused_very_long_name_that_probably_has_no_conflicts_but_even_if_it_starts_with_an_underscore_and_should_not_be_used_anyway, $reply, $error_code, $error_msg,);
    };

    ($option_in:ident, $reply:ident, $error_code:expr, $error_msg:expr, $($arg:tt)*) => {
        if $option_in.is_none() {
            error!($error_msg, $($arg)*);
            $reply.error($error_code);
            return;
        }
    };
}

#[macro_export]
macro_rules! reply_error_o {
    ($option_in:ident, $reply:ident, $error_code:expr, $error_msg:expr) => {
        reply_error_o!($option_in, $reply, $error_code, $error_msg,);
    };
    ($option_in:ident, $reply:ident, $error_code:expr, $error_msg:expr, $($arg:tt)*) => {
        if $option_in.is_none() {
            error!($error_msg, $($arg)*);
            $reply.error($error_code);
            return;
        }
        let $option_in = $option_in.unwrap();
    };
}

#[macro_export]
macro_rules! reply_error_e_consuming {
    ($result_in:ident, $reply:ident, $error_code:expr, $error_msg:expr) => {
        reply_error_e_consuming!($result_in, $reply, $error_code, $error_msg,);
    };

    ($result:ident, $reply:ident, $error_code:expr, $error_msg:expr, $($arg:tt)*) => {
        if let Err(e) = $result {
            error!("{}; e:{}",format!($error_msg, $($arg)*), e);
            $reply.error($error_code);
            return;
        }
    };
}
#[macro_export]
macro_rules! reply_error_e {
    ($result_in:ident, $reply:ident, $error_code:expr, $error_msg:expr) => {
        reply_error_e!($result_in, $reply, $error_code, $error_msg,);
    };
    ($result:ident, $reply:ident, $error_code:expr, $error_msg:expr, $($arg:tt)*) => {
        if let Err(e) = $result {
            error!("{}; e:{}",format!($error_msg, $($arg)*), e);
            $reply.error($error_code);
            return;
        }
        let $result = $result.unwrap();
    };
}
