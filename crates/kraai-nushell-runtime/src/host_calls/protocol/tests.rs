use super::*;
use kraai_execution_types::{CommandInvocationId, StateEffectAck};

fn ack() -> StateEffectAck {
    StateEffectAck {
        invocation_id: CommandInvocationId::new("invocation"),
        error: None,
    }
}

#[tokio::test]
async fn serialization_failure_preserves_errors_and_writes_no_frame() {
    struct InvalidPayload;

    impl serde::Serialize for InvalidPayload {
        fn serialize<S: serde::Serializer>(&self, _serializer: S) -> Result<S::Ok, S::Error> {
            Err(serde::ser::Error::custom("invalid effect payload"))
        }
    }

    let execution_id = ScriptExecutionId::new("execution");
    let secret = rand::random::<[u8; 32]>();
    let payload = InvalidPayload;
    let expected = match rmp_serde::to_vec_named(&FrameRef {
        execution_id: &execution_id,
        sequence: 1,
        payload: &payload,
    }) {
        Err(error) => error.to_string(),
        Ok(_) => panic!("invalid payload unexpectedly serialized"),
    };

    let mut synchronous = Vec::new();
    let mut asynchronous = Vec::new();
    let results = [
        write_authenticated_sync(&mut synchronous, &execution_id, 1, &payload, &secret),
        write_authenticated_async(&mut asynchronous, &execution_id, 1, &payload, &secret).await,
    ];
    for result in results {
        let Err(HostProtocolError::Serialize(error)) = result else {
            panic!("writer did not preserve serialization error");
        };
        assert_eq!(error.to_string(), expected);
    }
    assert!(synchronous.is_empty());
    assert!(asynchronous.is_empty());
}

#[tokio::test]
async fn frames_are_authenticated_before_they_are_decoded() {
    let execution_id = ScriptExecutionId::new("execution");
    let secret = rand::random::<[u8; 32]>();
    let mut wire = Vec::new();
    write_authenticated_sync(&mut wire, &execution_id, 1, &ack(), &secret).unwrap();
    let read = |wire: &[u8], execution_id: &str, secret: &[u8; 32]| {
        read_authenticated_sync::<StateEffectAck>(
            &mut &*wire,
            &ScriptExecutionId::new(execution_id),
            secret,
        )
    };
    assert!(read(&wire, "execution", &secret).is_ok());
    assert!(matches!(
        read(&wire, "other", &secret),
        Err(HostProtocolError::ExecutionId)
    ));
    assert!(matches!(
        read(&wire, "execution", &secret.map(|byte| byte ^ 1)),
        Err(HostProtocolError::Authentication(_))
    ));

    let mut garbage = vec![0xc1; 16];
    let mut hmac = Hmac::<sha2::Sha256>::new_from_slice(&secret.map(|byte| byte ^ 1)).unwrap();
    hmac.update(&garbage);
    let mut forged = u32::try_from(MAC_BYTES + garbage.len())
        .unwrap()
        .to_be_bytes()
        .to_vec();
    forged.extend(hmac.finalize().into_bytes());
    forged.append(&mut garbage);
    assert!(matches!(
        read(&forged, "execution", &secret),
        Err(HostProtocolError::Authentication(_))
    ));

    for index in LENGTH_BYTES..wire.len() {
        let mut tampered = wire.clone();
        if let Some(byte) = tampered.get_mut(index) {
            *byte ^= 1;
        }
        assert!(matches!(
            read_authenticated_async::<StateEffectAck>(
                &mut tampered.as_slice(),
                &execution_id,
                &secret
            )
            .await,
            Err(HostProtocolError::Authentication(_))
        ));
    }
}

#[tokio::test]
async fn authenticated_writers_preserve_frame_bytes() {
    let execution_id = ScriptExecutionId::new("execution");
    let secret = rand::random::<[u8; 32]>();
    let payload = ack();
    let body = rmp_serde::to_vec_named(&FrameRef {
        execution_id: &execution_id,
        sequence: 1,
        payload: &payload,
    })
    .unwrap();
    let mut hmac = Hmac::<sha2::Sha256>::new_from_slice(&secret).unwrap();
    hmac.update(&body);
    let mut expected = u32::try_from(MAC_BYTES + body.len())
        .unwrap()
        .to_be_bytes()
        .to_vec();
    expected.extend(hmac.finalize().into_bytes());
    expected.extend(body);

    let mut synchronous = Vec::new();
    write_authenticated_sync(&mut synchronous, &execution_id, 1, &payload, &secret).unwrap();
    assert_eq!(synchronous, expected);

    let mut asynchronous = Vec::new();
    write_authenticated_async(&mut asynchronous, &execution_id, 1, &payload, &secret)
        .await
        .unwrap();
    assert_eq!(asynchronous, expected);

    let (sequence, received) = read_authenticated_sync::<StateEffectAck>(
        &mut synchronous.as_slice(),
        &execution_id,
        &secret,
    )
    .unwrap();
    assert_eq!(sequence, 1);
    assert_eq!(received.invocation_id, payload.invocation_id);
    let frame = read_authenticated_async::<StateEffectAck>(
        &mut asynchronous.as_slice(),
        &execution_id,
        &secret,
    )
    .await
    .unwrap()
    .expect("missing frame");
    assert_eq!(frame.sequence, 1);
    assert_eq!(frame.payload.invocation_id, payload.invocation_id);
}

#[tokio::test]
async fn oversized_lengths_are_rejected_before_reading_the_payload() {
    let encoded = u32::try_from(MAX_FRAME_BYTES + 1).unwrap().to_be_bytes();
    let secret = rand::random::<[u8; 32]>();
    let mut synchronous = encoded.as_slice();
    let result = read_authenticated_sync::<serde_json::Value>(
        &mut synchronous,
        &ScriptExecutionId::new("image-test"),
        &secret,
    );
    assert!(matches!(result, Err(HostProtocolError::FrameTooLarge)));
    let mut asynchronous = encoded.as_slice();
    let result = read_authenticated_async::<serde_json::Value>(
        &mut asynchronous,
        &ScriptExecutionId::new("image-test"),
        &secret,
    )
    .await;
    assert!(matches!(result, Err(HostProtocolError::FrameTooLarge)));
}
