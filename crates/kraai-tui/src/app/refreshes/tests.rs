use super::*;

#[test]
fn list_refreshes_keep_one_request_in_flight_and_repeat_once() {
    let mut refreshes = Refreshes::default();
    assert!(refreshes.start(&RuntimeRequest::ListSessions));
    assert!(!refreshes.start(&RuntimeRequest::ListSessions));
    assert!(!refreshes.start(&RuntimeRequest::ListSessions));
    assert!(refreshes.start(&RuntimeRequest::ListUserInputHistory { limit: 1 }));
    assert!(refreshes.start(&RuntimeRequest::ListModels));
    assert!(refreshes.start(&RuntimeRequest::ListModels));

    assert!(matches!(
        refreshes.finish(&RuntimeResponse::Sessions(Ok(Vec::new()))),
        FinishedRefresh::Repeat(RuntimeRequest::ListSessions)
    ));
    assert!(matches!(
        refreshes.finish(&RuntimeResponse::UserInputHistory(Ok(Vec::new()))),
        FinishedRefresh::None
    ));
    assert!(refreshes.start(&RuntimeRequest::ListSessions));
    assert!(matches!(
        refreshes.finish(&RuntimeResponse::Sessions(Ok(Vec::new()))),
        FinishedRefresh::None
    ));
}
