use lilia_core::{BatchOperation, MutationResult};
use serde_json::json;

#[test]
fn binary_versions_remain_native_u64_and_json_versions_are_decimal() {
    for value in [0, 1, (1_u64 << 53) + 1, u64::MAX] {
        let operation = BatchOperation::JsonDelete {
            space: "v".into(),
            id: "a".into(),
            if_version: Some(value),
        };
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&operation).unwrap();
        let archived =
            rkyv::access::<lilia_core::ArchivedBatchOperation, rkyv::rancor::Error>(&bytes)
                .unwrap();
        match archived {
            lilia_core::ArchivedBatchOperation::JsonDelete { if_version, .. } => {
                assert_eq!(if_version.as_ref().map(|v| v.to_native()), Some(value));
            }
            _ => panic!("unexpected variant"),
        }
        let decoded: BatchOperation =
            rkyv::from_bytes::<BatchOperation, rkyv::rancor::Error>(&bytes).unwrap();
        assert_eq!(
            serde_json::to_value(decoded).unwrap()["if_version"],
            value.to_string()
        );
        let request = lilia_protocol::Request {
            id: "test".into(),
            token: "test".into(),
            deadline_ms: Some(5000),
            operation: lilia_protocol::Operation::Batch {
                operations: vec![operation],
            },
        };
        let request_bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&request).unwrap();
        let _: lilia_protocol::Request =
            rkyv::from_bytes::<lilia_protocol::Request, rkyv::rancor::Error>(&request_bytes)
                .unwrap();
        let result = MutationResult {
            version: Some(value),
            deleted: false,
        };
        let result_bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&result).unwrap();
        let archived_res =
            rkyv::access::<lilia_core::ArchivedMutationResult, rkyv::rancor::Error>(&result_bytes)
                .unwrap();
        assert_eq!(
            archived_res.version.as_ref().map(|v| v.to_native()),
            Some(value)
        );
        assert_eq!(
            serde_json::to_value(&result).unwrap(),
            json!({"version": value.to_string(), "deleted": false})
        );
    }
}
