//! A JSON document as the private witness of the `solde_age` circuit: its integer fields
//! `balance` and `age`, and a salt. Its commitment and its proofs come from the same witness,
//! so a proof answers for the commitment published before it.

use std::collections::BTreeMap;

use ark_bn254::Fr;
use serde_json::Value;

use crate::{
    fr_to_hex, hex_to_fr, poseidon_hash_4, str_to_fr, u64_to_fr, ProvingKey, Result, ZkError,
    ZkProofBundle, ZkProver, ZkStatement,
};

/// The witness of document `id`.
#[derive(Debug, Clone)]
pub struct DocumentWitness {
    id: String,
    balance: u64,
    age: u64,
    salt: Fr,
}

/// `text` as a field element: its value when it is hexadecimal (`0x` optional), else its hash.
fn salt_of(text: &str) -> Fr {
    hex_to_fr(text).unwrap_or_else(|_| str_to_fr(text))
}

fn field(document: &Value, name: &str) -> Result<u64> {
    document
        .get(name)
        .and_then(Value::as_u64)
        .ok_or_else(|| ZkError::MissingWitness(format!("{name} (an unsigned integer)")))
}

impl DocumentWitness {
    /// The witness of document `id` of `space`. The salt is `salt`, else the document's
    /// `_salt` or `salt` field, else `"<space>:<id>"`.
    ///
    /// # Errors
    ///
    /// [`ZkError::MissingWitness`] when `balance` or `age` is not an unsigned integer.
    pub fn new(space: &str, id: &str, document: &Value, salt: Option<&str>) -> Result<Self> {
        let salt = salt
            .or_else(|| document.get("_salt").and_then(Value::as_str))
            .or_else(|| document.get("salt").and_then(Value::as_str))
            .map_or_else(|| str_to_fr(&format!("{space}:{id}")), salt_of);
        Ok(Self {
            id: id.to_owned(),
            balance: field(document, "balance")?,
            age: field(document, "age")?,
            salt,
        })
    }

    /// The Poseidon commitment to the document, as `0x`-prefixed hex.
    #[must_use]
    pub fn commitment(&self) -> String {
        fr_to_hex(&poseidon_hash_4(
            str_to_fr(&self.id),
            u64_to_fr(self.balance),
            u64_to_fr(self.age),
            self.salt,
        ))
    }

    /// A proof that the document satisfies `statement`, with the proving key `pk` (the
    /// circuit's default keys otherwise).
    ///
    /// # Errors
    ///
    /// A statement the circuit does not support, or a failure to prove.
    pub fn prove(&self, statement: &ZkStatement, pk: Option<&ProvingKey>) -> Result<ZkProofBundle> {
        let witness = BTreeMap::from([
            ("balance".to_owned(), self.balance),
            ("age".to_owned(), self.age),
        ]);
        ZkProver::prove_statement(&self.id, statement, &witness, self.salt, pk)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::ZkVerifier;

    #[test]
    fn a_proof_answers_for_the_published_commitment() {
        let document = json!({"balance": 5_000, "age": 34, "_salt": "0x2a"});
        let witness = DocumentWitness::new("accounts", "a1", &document, None).unwrap();
        let statement = ZkStatement::builder()
            .field_gte("balance", 1_000)
            .field_gte("age", 18)
            .build();
        let bundle = witness.prove(&statement, None).unwrap();
        assert_eq!(bundle.commitment_hex, witness.commitment());
        assert!(ZkVerifier::verify(&bundle, None).unwrap().valid);

        let other_salt = DocumentWitness::new("accounts", "a1", &document, Some("pepper")).unwrap();
        assert_ne!(other_salt.commitment(), witness.commitment());
        assert!(matches!(
            DocumentWitness::new("accounts", "a1", &json!({"balance": 1}), None),
            Err(ZkError::MissingWitness(_))
        ));
    }
}
