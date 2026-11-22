use lilia_protocol::{read_frame, write_frame, Operation, Request, PROTOCOL_MAJOR};

#[tokio::test]
async fn frame_round_trip() {
    let (mut client, mut server) = tokio::io::duplex(4_096);
    let request = Request {
        id: "018f0000-0000-7000-8000-000000000001".into(),
        token: "secret".into(),
        deadline_ms: Some(1_000),
        operation: Operation::Handshake {
            major: PROTOCOL_MAJOR,
            minor: 0,
        },
    };
    let expected_id = request.id.clone();
    let writer = tokio::spawn(async move { write_frame(&mut client, &request).await });
    let decoded: Request = read_frame(&mut server).await.expect("decode request");
    writer.await.expect("writer task").expect("write request");
    assert_eq!(decoded.id, expected_id);
    assert!(matches!(
        decoded.operation,
        Operation::Handshake {
            major: PROTOCOL_MAJOR,
            minor: 0
        }
    ));
}

#[tokio::test]
async fn rejects_empty_frame() {
    use tokio::io::AsyncWriteExt;

    let (mut client, mut server) = tokio::io::duplex(4);
    client.write_u32(0).await.expect("write empty length");
    let error = read_frame::<Request>(&mut server)
        .await
        .expect_err("empty frame rejected");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
}

#[tokio::test]
async fn responses_of_the_storage_round_trip() {
    use lilia_db::{LevelStats, StorageStats, UndoStep};
    use lilia_protocol::{Response, ResponseValue};

    let (mut client, mut server) = tokio::io::duplex(4_096);
    let stats = StorageStats {
        levels: vec![LevelStats {
            runs: 2,
            bytes: 4_096,
        }],
        memtable_bytes: 512,
        immutable_memtables: 0,
        version: 42,
        oldest_version: 7,
    };
    let responses = [
        ResponseValue::StorageStats(stats.clone()),
        ResponseValue::UndoStep(Some(UndoStep {
            version: 9,
            keys: 3,
        })),
    ];
    for value in responses {
        let response = Response {
            id: "r".into(),
            result: Ok(value),
        };
        write_frame(&mut server, &response)
            .await
            .expect("write response");
        let decoded: Response = read_frame(&mut client).await.expect("read response");
        match decoded.result.unwrap() {
            ResponseValue::StorageStats(decoded) => assert_eq!(decoded, stats),
            ResponseValue::UndoStep(step) => {
                assert_eq!(
                    step,
                    Some(UndoStep {
                        version: 9,
                        keys: 3
                    })
                );
            }
            other => panic!("unexpected response value: {other:?}"),
        }
    }
}

#[derive(Debug, PartialEq, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[rkyv(crate = rkyv, derive(Debug, PartialEq))]
struct FastIpcMessage {
    pub session_id: u64,
    pub command: String,
    pub payload: Vec<u8>,
}

#[tokio::test]
async fn rkyv_zero_copy_frame_round_trip() {
    use lilia_protocol::{read_archived_frame, write_archived_frame};
    use rkyv::util::AlignedVec;

    let (mut client, mut server) = tokio::io::duplex(4_096);
    let msg = FastIpcMessage {
        session_id: 10007,
        command: "QUERY_AGGREGATE".to_string(),
        payload: vec![1, 2, 3, 4, 5],
    };

    let writer = tokio::spawn(async move {
        write_archived_frame(&mut client, &msg)
            .await
            .expect("write archived frame");
    });

    let mut buf = AlignedVec::new();
    let archived = read_archived_frame::<FastIpcMessage>(&mut server, &mut buf)
        .await
        .expect("read archived frame");

    writer.await.expect("writer task");

    // Zero-copy in-situ assertions without deserialization!
    assert_eq!(archived.session_id.to_native(), 10007);
    assert_eq!(archived.command.as_str(), "QUERY_AGGREGATE");
    assert_eq!(archived.payload.as_slice(), &[1, 2, 3, 4, 5]);
}

/// A batch request whose key and condition test the byte and `u64` encodings.
fn batch_request() -> Request {
    use lilia_core::BatchOperation;
    Request {
        id: "r1".into(),
        token: "tok".into(),
        deadline_ms: None,
        operation: Operation::Batch {
            operations: vec![BatchOperation::KvSet {
                namespace: "ns".into(),
                key: vec![0, 159, 255],
                value: b"v".to_vec(),
                if_version: Some(u64::MAX),
                expires_at_ms: None,
            }],
        },
    }
}

fn assert_batch(decoded: Request) {
    use lilia_core::BatchOperation;
    let Operation::Batch { operations } = decoded.operation else {
        panic!("the batch survives");
    };
    assert!(matches!(
        &operations[0],
        BatchOperation::KvSet { key, if_version: Some(u64::MAX), .. } if key == &[0, 159, 255]
    ));
}

#[tokio::test]
async fn rkyv_clients_exchange_archives_after_their_preface() {
    use lilia_core::{ErrorCode, LiliaError};
    use lilia_protocol::{Codec, Response};

    let (mut client, mut server) = tokio::io::duplex(8_192);
    Codec::Rkyv.announce(&mut client).await.unwrap();
    write_frame(&mut client, &batch_request()).await.unwrap();
    let codec = Codec::accept(&mut server).await.unwrap();
    assert_eq!(codec, Codec::Rkyv);
    assert_batch(codec.read_request(&mut server).await.unwrap());
    let response = Response {
        id: "r1".into(),
        result: Err(LiliaError::new(ErrorCode::Conflict, "stale", true)),
    };
    codec.write_response(&mut server, &response).await.unwrap();
    let decoded: Response = read_frame(&mut client).await.unwrap();
    let error = decoded.result.unwrap_err();
    assert_eq!((error.code, error.retryable), (ErrorCode::Conflict, true));

    let (mut client, mut server) = tokio::io::duplex(64);
    tokio::io::AsyncWriteExt::write_all(&mut client, b"HTTP")
        .await
        .unwrap();
    let refused = Codec::accept(&mut server).await.unwrap_err();
    assert_eq!(refused.kind(), std::io::ErrorKind::InvalidData);
}

#[tokio::test]
async fn js_clients_send_message_pack_and_receive_js_frames() {
    use lilia_core::KvEntry;
    use lilia_protocol::{to_msgpack, Codec, Response, ResponseValue};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let (mut client, mut server) = tokio::io::duplex(8_192);
    client.write_all(&Codec::Js.preface()).await.unwrap();
    let request = to_msgpack(&batch_request()).unwrap();
    client.write_u32(request.len() as u32).await.unwrap();
    client.write_all(&request).await.unwrap();
    let codec = Codec::accept(&mut server).await.unwrap();
    assert_eq!(codec, Codec::Js);
    assert_batch(codec.read_request(&mut server).await.unwrap());

    let response = Response {
        id: "r1".into(),
        result: Ok(ResponseValue::Kv(Some(KvEntry {
            namespace: "ns".into(),
            key: vec![0, 159, 255],
            value: b"v".to_vec(),
            version: u64::MAX,
            expires_at_ms: None,
        }))),
    };
    codec.write_response(&mut server, &response).await.unwrap();
    let length = client.read_u32().await.unwrap() as usize;
    let mut frame = vec![0; length];
    client.read_exact(&mut frame).await.unwrap();
    let json = u32::from_le_bytes(frame[..4].try_into().unwrap()) as usize;
    let value: serde_json::Value = serde_json::from_slice(&frame[4..4 + json]).unwrap();
    let entry = &value["result"]["Ok"]["value"];
    assert_eq!(value["result"]["Ok"]["type"], "kv");
    assert_eq!(
        entry["version"],
        u64::MAX.to_string(),
        "versions are decimal strings"
    );
    let segments = 8 + json;
    let key = entry["key"].as_u64().unwrap() as usize;
    let at = |i: usize, field: usize| {
        let offset = segments + i * 8 + field * 4;
        u32::from_le_bytes(frame[offset..offset + 4].try_into().unwrap()) as usize
    };
    let bytes = segments + 2 * 8;
    assert_eq!(
        &frame[bytes + at(key, 0)..bytes + at(key, 0) + at(key, 1)],
        &[0, 159, 255]
    );
}
