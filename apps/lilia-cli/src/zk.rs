//! Zero-knowledge proofs on documents (circuit `solde_age`): the keys, a document's
//! commitment, a proof of conditions on it, and its verification. Only the commitment and the
//! proof read the database.

use std::fs;
use std::path::Path;

use lilia_core::LiliaError;
use lilia_zk::{DocumentWitness, ZkProofBundle, ZkStatement, ZkVerifier};
use serde_json::{json, Value};

use crate::args::{OpenArgs, ZkCommand};
use crate::models::{self, input::invalid};

pub(crate) fn execute(
    command: ZkCommand,
    path: &Path,
    options: &OpenArgs,
) -> anyhow::Result<Value> {
    match command {
        ZkCommand::Setup { out_pk, out_vk } => {
            let (pk, vk) =
                lilia_zk::generate_circuit_keys(lilia_zk::SoldeAgePredicateCircuit::empty())?;
            fs::write(&out_pk, lilia_zk::serialize_pk(&pk)?)?;
            fs::write(&out_vk, lilia_zk::serialize_vk(&vk)?)?;
            Ok(json!({"circuit": "solde_age_v1", "pk_file": out_pk, "vk_file": out_vk}))
        }
        ZkCommand::Commit { space, id, salt } => {
            let witness = witness(path, options, &space, &id, salt.as_deref())?;
            Ok(json!({"space": space, "id": id, "commitment": witness.commitment()}))
        }
        ZkCommand::Prove {
            space,
            id,
            condition,
            salt,
            pk,
        } => {
            let statement = statement(&condition)?;
            let pk = key(pk.as_deref(), lilia_zk::deserialize_pk)?;
            let witness = witness(path, options, &space, &id, salt.as_deref())?;
            Ok(models::render(&witness.prove(&statement, pk.as_ref())?)?)
        }
        ZkCommand::Verify { proof, vk } => {
            let bundle: ZkProofBundle = serde_json::from_slice(&fs::read(proof)?)?;
            let vk = key(vk.as_deref(), lilia_zk::deserialize_vk)?;
            Ok(models::render(&ZkVerifier::verify(&bundle, vk.as_ref())?)?)
        }
    }
}

/// The witness of document `id` of `space`, read from the database.
fn witness(
    path: &Path,
    options: &OpenArgs,
    space: &str,
    id: &str,
    salt: Option<&str>,
) -> anyhow::Result<DocumentWitness> {
    let database = models::open(path, options)?;
    let document = database
        .system()
        .json_get(space, id)
        .map_err(LiliaError::from);
    database.close().map_err(LiliaError::from)?;
    let document = document?.ok_or_else(|| {
        LiliaError::new(
            lilia_core::ErrorCode::NotFound,
            format!("no document {id} in {space}"),
            false,
        )
    })?;
    Ok(DocumentWitness::new(space, id, &document.value(), salt)?)
}

/// The key of `file`, when one is given.
fn key<T>(
    file: Option<&Path>,
    parse: fn(&[u8]) -> lilia_zk::Result<T>,
) -> anyhow::Result<Option<T>> {
    file.map(|file| Ok(parse(&fs::read(file)?)?)).transpose()
}

/// The claims of `condition`: `field >= value` terms joined by `AND`.
fn statement(condition: &str) -> Result<ZkStatement, LiliaError> {
    let mut builder = ZkStatement::builder();
    for term in condition.split("AND") {
        let (field, threshold) = term
            .split_once(">=")
            .ok_or_else(|| invalid(format!("`{}` is not a `field >= value` claim", term.trim())))?;
        let threshold = threshold
            .trim()
            .parse::<u64>()
            .map_err(|e| invalid(format!("`{}`: {e}", term.trim())))?;
        builder = builder.field_gte(field.trim(), threshold);
    }
    Ok(builder.build())
}
