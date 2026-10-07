use kraai_io::http::{HTTP_FINITE_REQUEST_TIMEOUT, read_response_text_prefix};

const MAX_ERROR_BODY_BYTES: usize = 64 * 1024;

pub async fn read_error_body(response: reqwest::Response) -> String {
    match tokio::time::timeout(
        HTTP_FINITE_REQUEST_TIMEOUT,
        read_response_text_prefix(response, MAX_ERROR_BODY_BYTES),
    )
    .await
    {
        Ok(Ok(body)) => {
            let mut text = body.text;
            if body.truncated {
                text.push_str("\n<response body truncated at 64 KiB>");
            }
            text
        }
        Ok(Err(error)) => format!("<failed to read body: {error}>"),
        Err(_error) => String::from("<timed out reading error body>"),
    }
}
