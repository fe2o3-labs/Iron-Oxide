//! Target-independent WebAuthn conversions between the `webauthn-rs-proto` types exchanged with
//! the server and what `navigator.credentials.create()/get()` take and return.
//!
//! The browser wants the binary option fields (`challenge`, `user.id`, credential ids) as
//! `BufferSource`s, not the base64url strings the JSON form uses. [`creation_options`] and
//! [`request_options`] return the options as JSON plus the list of binary fields; the wasm glue
//! (`auth/browser.rs`) parses the JSON and sets a `Uint8Array` at each listed path.
//!
//! In the other direction the glue reads the credential's `ArrayBuffer`s into plain bytes, and
//! [`registration_credential`] / [`assertion_credential`] build the proto structs from them.
//! Only the client extension outputs the server uses are forwarded (`credProps.rk`), so an
//! unexpected output from some browser cannot make the whole credential fail to deserialize.

use serde_json::Value;
use webauthn_rs_proto::{
    AuthenticationExtensionsClientOutputs, AuthenticatorAssertionResponseRaw,
    AuthenticatorAttestationResponseRaw, AuthenticatorTransport, CreationChallengeResponse,
    CredProps, PublicKeyCredential, RegisterPublicKeyCredential,
    RegistrationExtensionsClientOutputs, RequestChallengeResponse,
};

/// The only credential type WebAuthn defines.
const PUBLIC_KEY_TYPE: &str = "public-key";

/// One step of a path into the options JSON.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathStep {
    /// An object member.
    Key(&'static str),
    /// An array element.
    Index(usize),
}

/// A binary field of the options: where it is in the JSON, and its bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinaryField {
    pub path: Vec<PathStep>,
    pub bytes: Vec<u8>,
}

/// Options ready for the browser: the JSON, and the binary fields to replace in it.
#[derive(Debug, Clone, PartialEq)]
pub struct BrowserOptions {
    /// The options as JSON. Each [`BinaryField`] path points to its base64url string here.
    pub json: Value,
    /// The fields the glue must replace with a `Uint8Array` of their bytes.
    pub binary_fields: Vec<BinaryField>,
}

/// Why a conversion failed. Never expected with a well-behaved server and browser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversionError(pub String);

impl std::fmt::Display for ConversionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ConversionError {}

fn to_json<T: serde::Serialize>(value: &T) -> Result<Value, ConversionError> {
    serde_json::to_value(value).map_err(|e| ConversionError(format!("options to JSON: {e}")))
}

/// The options for `navigator.credentials.create()`.
pub fn creation_options(
    options: &CreationChallengeResponse,
) -> Result<BrowserOptions, ConversionError> {
    let json = to_json(options)?;
    let public_key = &options.public_key;
    let mut binary_fields = vec![
        BinaryField {
            path: vec![PathStep::Key("publicKey"), PathStep::Key("challenge")],
            bytes: public_key.challenge.to_vec(),
        },
        BinaryField {
            path: vec![
                PathStep::Key("publicKey"),
                PathStep::Key("user"),
                PathStep::Key("id"),
            ],
            bytes: public_key.user.id.to_vec(),
        },
    ];
    for (index, credential) in public_key.exclude_credentials.iter().flatten().enumerate() {
        binary_fields.push(BinaryField {
            path: vec![
                PathStep::Key("publicKey"),
                PathStep::Key("excludeCredentials"),
                PathStep::Index(index),
                PathStep::Key("id"),
            ],
            bytes: credential.id.to_vec(),
        });
    }
    Ok(BrowserOptions {
        json,
        binary_fields,
    })
}

/// The options for `navigator.credentials.get()`.
///
/// Drops the `hmacGetSecret` extension input if present: it is not a browser extension (and
/// holds binary data the browser would reject as a string). The server never requests it.
pub fn request_options(
    options: &RequestChallengeResponse,
) -> Result<BrowserOptions, ConversionError> {
    let mut json = to_json(options)?;
    if let Some(extensions) = json
        .get_mut("publicKey")
        .and_then(|public_key| public_key.get_mut("extensions"))
        .and_then(Value::as_object_mut)
    {
        extensions.remove("hmacGetSecret");
    }
    let public_key = &options.public_key;
    let mut binary_fields = vec![BinaryField {
        path: vec![PathStep::Key("publicKey"), PathStep::Key("challenge")],
        bytes: public_key.challenge.to_vec(),
    }];
    for (index, credential) in public_key.allow_credentials.iter().enumerate() {
        binary_fields.push(BinaryField {
            path: vec![
                PathStep::Key("publicKey"),
                PathStep::Key("allowCredentials"),
                PathStep::Index(index),
                PathStep::Key("id"),
            ],
            bytes: credential.id.to_vec(),
        });
    }
    Ok(BrowserOptions {
        json,
        binary_fields,
    })
}

/// What the browser returned from `navigator.credentials.create()`, as plain data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawRegistration {
    pub raw_id: Vec<u8>,
    pub client_data_json: Vec<u8>,
    pub attestation_object: Vec<u8>,
    /// `response.getTransports()`, when the browser has it.
    pub transports: Option<Vec<String>>,
    /// `getClientExtensionResults().credProps.rk`, when reported.
    pub cred_props_rk: Option<bool>,
}

/// What the browser returned from `navigator.credentials.get()`, as plain data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawAssertion {
    pub raw_id: Vec<u8>,
    pub client_data_json: Vec<u8>,
    pub authenticator_data: Vec<u8>,
    pub signature: Vec<u8>,
    /// `None` when the browser returned `null` (or no field).
    pub user_handle: Option<Vec<u8>>,
}

/// The base64url (no padding) string a binary proto field serializes to.
fn base64url_of<T: serde::Serialize>(field: &T) -> Result<String, ConversionError> {
    match to_json(field)? {
        Value::String(encoded) => Ok(encoded),
        other => Err(ConversionError(format!("binary field encoded as {other}"))),
    }
}

fn require_non_empty(name: &str, bytes: &[u8]) -> Result<(), ConversionError> {
    if bytes.is_empty() {
        return Err(ConversionError(format!(
            "the browser returned an empty {name}"
        )));
    }
    Ok(())
}

/// The credential to send to `passkey_sign_up_finish` / `passkey_add_finish`.
pub fn registration_credential(
    raw: RawRegistration,
) -> Result<RegisterPublicKeyCredential, ConversionError> {
    require_non_empty("rawId", &raw.raw_id)?;
    require_non_empty("clientDataJSON", &raw.client_data_json)?;
    require_non_empty("attestationObject", &raw.attestation_object)?;
    let transports = raw.transports.map(|transports| {
        transports
            .into_iter()
            .filter_map(|transport| {
                // Unknown names become `AuthenticatorTransport::Unknown`.
                serde_json::from_value::<AuthenticatorTransport>(Value::String(transport)).ok()
            })
            .collect()
    });
    let mut credential = RegisterPublicKeyCredential {
        id: String::new(),
        raw_id: raw.raw_id.into(),
        response: AuthenticatorAttestationResponseRaw {
            attestation_object: raw.attestation_object.into(),
            client_data_json: raw.client_data_json.into(),
            transports,
        },
        type_: PUBLIC_KEY_TYPE.to_owned(),
        extensions: RegistrationExtensionsClientOutputs {
            cred_props: raw.cred_props_rk.map(|rk| CredProps { rk: Some(rk) }),
            ..RegistrationExtensionsClientOutputs::default()
        },
    };
    credential.id = base64url_of(&credential.raw_id)?;
    Ok(credential)
}

/// The credential to send to `passkey_sign_in_finish`.
pub fn assertion_credential(raw: RawAssertion) -> Result<PublicKeyCredential, ConversionError> {
    require_non_empty("rawId", &raw.raw_id)?;
    require_non_empty("clientDataJSON", &raw.client_data_json)?;
    require_non_empty("authenticatorData", &raw.authenticator_data)?;
    require_non_empty("signature", &raw.signature)?;
    let mut credential = PublicKeyCredential {
        id: String::new(),
        raw_id: raw.raw_id.into(),
        response: AuthenticatorAssertionResponseRaw {
            authenticator_data: raw.authenticator_data.into(),
            client_data_json: raw.client_data_json.into(),
            signature: raw.signature.into(),
            // A present user handle is never empty; treat an empty one as absent.
            user_handle: raw
                .user_handle
                .filter(|handle| !handle.is_empty())
                .map(Into::into),
        },
        extensions: AuthenticationExtensionsClientOutputs::default(),
        type_: PUBLIC_KEY_TYPE.to_owned(),
    };
    credential.id = base64url_of(&credential.raw_id)?;
    Ok(credential)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// base64url without padding, written out by hand so the tests do not share the code under
    /// test.
    fn b64url(bytes: &[u8]) -> String {
        const ALPHABET: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
        let mut out = String::new();
        for chunk in bytes.chunks(3) {
            let b = [
                chunk[0],
                *chunk.get(1).unwrap_or(&0),
                *chunk.get(2).unwrap_or(&0),
            ];
            let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
            let chars = chunk.len() + 1;
            for i in 0..chars {
                out.push(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize] as char);
            }
        }
        out
    }

    fn at<'a>(json: &'a Value, path: &[PathStep]) -> &'a Value {
        path.iter().fold(json, |value, step| match step {
            PathStep::Key(key) => &value[*key],
            PathStep::Index(index) => &value[*index],
        })
    }

    /// Every binary field points at a base64url string holding exactly its bytes.
    fn assert_binary_fields_match(options: &BrowserOptions) {
        for field in &options.binary_fields {
            let value = at(&options.json, &field.path);
            assert_eq!(
                value.as_str(),
                Some(b64url(&field.bytes).as_str()),
                "{:?}",
                field.path
            );
        }
    }

    fn creation_response(exclude: Option<Value>) -> CreationChallengeResponse {
        let mut public_key = json!({
            "rp": { "name": "Iron Oxide", "id": "localhost" },
            "user": { "id": b64url(&[1, 2, 3, 250]), "name": "jules", "displayName": "Jules" },
            "challenge": b64url(&[9; 32]),
            "pubKeyCredParams": [{ "type": "public-key", "alg": -7 }],
            "timeout": 60000,
            "authenticatorSelection": {
                "residentKey": "required",
                "requireResidentKey": true,
                "userVerification": "required"
            },
            "attestation": "none",
            "extensions": { "credentialProtectionPolicy": "userVerificationRequired",
                            "uvm": true, "credProps": true }
        });
        if let Some(exclude) = exclude {
            public_key["excludeCredentials"] = exclude;
        }
        serde_json::from_value(json!({ "publicKey": public_key })).unwrap()
    }

    fn request_response(allow: Value, extensions: Option<Value>) -> RequestChallengeResponse {
        let mut public_key = json!({
            "challenge": b64url(&[7; 32]),
            "timeout": 60000,
            "rpId": "localhost",
            "allowCredentials": allow,
            "userVerification": "required"
        });
        if let Some(extensions) = extensions {
            public_key["extensions"] = extensions;
        }
        serde_json::from_value(json!({ "publicKey": public_key })).unwrap()
    }

    #[test]
    fn creation_options_list_challenge_and_user_id() {
        let options = creation_options(&creation_response(None)).unwrap();
        assert_eq!(
            options.binary_fields,
            vec![
                BinaryField {
                    path: vec![PathStep::Key("publicKey"), PathStep::Key("challenge")],
                    bytes: vec![9; 32],
                },
                BinaryField {
                    path: vec![
                        PathStep::Key("publicKey"),
                        PathStep::Key("user"),
                        PathStep::Key("id")
                    ],
                    bytes: vec![1, 2, 3, 250],
                },
            ]
        );
        assert_binary_fields_match(&options);
    }

    #[test]
    fn creation_options_list_every_excluded_credential() {
        let exclude = json!([
            { "type": "public-key", "id": b64url(&[1; 16]) },
            { "type": "public-key", "id": b64url(&[2; 20]), "transports": ["internal", "hybrid"] }
        ]);
        let options = creation_options(&creation_response(Some(exclude))).unwrap();
        assert_eq!(options.binary_fields.len(), 4);
        assert_eq!(options.binary_fields[2].bytes, vec![1; 16]);
        assert_eq!(options.binary_fields[3].bytes, vec![2; 20]);
        assert_eq!(
            options.binary_fields[3].path,
            vec![
                PathStep::Key("publicKey"),
                PathStep::Key("excludeCredentials"),
                PathStep::Index(1),
                PathStep::Key("id"),
            ]
        );
        assert_binary_fields_match(&options);
        assert_eq!(
            options.json["publicKey"]["excludeCredentials"][1]["transports"],
            json!(["internal", "hybrid"])
        );
    }

    #[test]
    fn creation_options_keep_extensions_and_selection_as_json() {
        let options = creation_options(&creation_response(None)).unwrap();
        let public_key = &options.json["publicKey"];
        assert_eq!(public_key["extensions"]["credProps"], json!(true));
        assert_eq!(public_key["extensions"]["uvm"], json!(true));
        assert_eq!(
            public_key["extensions"]["credentialProtectionPolicy"],
            json!("userVerificationRequired")
        );
        assert_eq!(
            public_key["authenticatorSelection"]["residentKey"],
            json!("required")
        );
        assert_eq!(public_key["rp"]["id"], json!("localhost"));
        assert_eq!(public_key["user"]["displayName"], json!("Jules"));
        assert_eq!(public_key["pubKeyCredParams"][0]["alg"], json!(-7));
        assert!(public_key.get("excludeCredentials").is_none());
    }

    #[test]
    fn request_options_list_challenge_and_allowed_credentials() {
        let options = request_options(&request_response(json!([]), None)).unwrap();
        assert_eq!(options.binary_fields.len(), 1);
        assert_eq!(options.binary_fields[0].bytes, vec![7; 32]);
        assert_binary_fields_match(&options);
        assert!(options.json.get("mediation").is_none());

        let allow = json!([
            { "type": "public-key", "id": b64url(&[3; 16]) },
            { "type": "public-key", "id": b64url(&[4, 5]) }
        ]);
        let options = request_options(&request_response(allow, None)).unwrap();
        assert_eq!(options.binary_fields.len(), 3);
        assert_eq!(options.binary_fields[2].bytes, vec![4, 5]);
        assert_binary_fields_match(&options);
    }

    #[test]
    fn request_options_keep_mediation_when_set() {
        let mut response = request_response(json!([]), None);
        response.mediation = Some(webauthn_rs_proto::Mediation::Conditional);
        let options = request_options(&response).unwrap();
        assert_eq!(options.json["mediation"], json!("conditional"));
    }

    #[test]
    fn request_options_drop_hmac_get_secret_and_keep_other_extensions() {
        let extensions = json!({
            "uvm": true,
            "appid": "https://example.com",
            "hmacGetSecret": { "output1": b64url(&[1; 32]) }
        });
        let options = request_options(&request_response(json!([]), Some(extensions))).unwrap();
        let extensions = &options.json["publicKey"]["extensions"];
        assert!(extensions.get("hmacGetSecret").is_none());
        assert_eq!(extensions["uvm"], json!(true));
        assert_eq!(extensions["appid"], json!("https://example.com"));
    }

    fn raw_registration() -> RawRegistration {
        RawRegistration {
            raw_id: vec![0xfb, 0xff, 0x01],
            client_data_json: b"{\"type\":\"webauthn.create\"}".to_vec(),
            attestation_object: vec![0xa3, 1, 2],
            transports: Some(vec!["internal".to_owned(), "hybrid".to_owned()]),
            cred_props_rk: Some(true),
        }
    }

    #[test]
    fn registration_credential_encodes_every_field_as_base64url() {
        let credential = registration_credential(raw_registration()).unwrap();
        let json = serde_json::to_value(&credential).unwrap();
        assert_eq!(json["id"], json!("-_8B"));
        assert_eq!(json["rawId"], json!("-_8B"));
        assert_eq!(json["type"], json!("public-key"));
        assert_eq!(
            json["response"]["clientDataJSON"],
            json!(b64url(b"{\"type\":\"webauthn.create\"}"))
        );
        assert_eq!(
            json["response"]["attestationObject"],
            json!(b64url(&[0xa3, 1, 2]))
        );
        assert_eq!(
            json["response"]["transports"],
            json!(["internal", "hybrid"])
        );
        assert_eq!(json["extensions"], json!({ "credProps": { "rk": true } }));
    }

    #[test]
    fn registration_credential_round_trips_through_the_server_json() {
        let credential = registration_credential(raw_registration()).unwrap();
        let json = serde_json::to_string(&credential).unwrap();
        let back: RegisterPublicKeyCredential = serde_json::from_str(&json).unwrap();
        assert_eq!(back.raw_id, vec![0xfb, 0xff, 0x01]);
        assert_eq!(back.response.attestation_object, vec![0xa3, 1, 2]);
        assert_eq!(back.extensions.cred_props.and_then(|p| p.rk), Some(true));
    }

    #[test]
    fn registration_credential_reports_missing_cred_props_and_transports_as_none() {
        let credential = registration_credential(RawRegistration {
            transports: None,
            cred_props_rk: None,
            ..raw_registration()
        })
        .unwrap();
        assert!(credential.extensions.cred_props.is_none());
        assert!(credential.response.transports.is_none());

        let credential = registration_credential(RawRegistration {
            cred_props_rk: Some(false),
            ..raw_registration()
        })
        .unwrap();
        assert_eq!(
            credential.extensions.cred_props.and_then(|p| p.rk),
            Some(false)
        );
    }

    #[test]
    fn registration_credential_keeps_unknown_transports_as_unknown() {
        let credential = registration_credential(RawRegistration {
            transports: Some(vec!["usb".to_owned(), "carrier-pigeon".to_owned()]),
            ..raw_registration()
        })
        .unwrap();
        let transports = credential.response.transports.unwrap();
        assert_eq!(transports.len(), 2);
        assert!(matches!(transports[0], AuthenticatorTransport::Usb));
        assert!(matches!(transports[1], AuthenticatorTransport::Unknown));
    }

    #[test]
    fn registration_credential_rejects_empty_buffers() {
        for raw in [
            RawRegistration {
                raw_id: vec![],
                ..raw_registration()
            },
            RawRegistration {
                client_data_json: vec![],
                ..raw_registration()
            },
            RawRegistration {
                attestation_object: vec![],
                ..raw_registration()
            },
        ] {
            assert!(registration_credential(raw).is_err());
        }
    }

    fn raw_assertion() -> RawAssertion {
        RawAssertion {
            raw_id: vec![1, 2, 3, 4, 5],
            client_data_json: b"{\"type\":\"webauthn.get\"}".to_vec(),
            authenticator_data: vec![0x49; 37],
            signature: vec![0x30, 0x45, 0xff],
            user_handle: Some(vec![0xde, 0xad, 0xbe, 0xef]),
        }
    }

    #[test]
    fn assertion_credential_encodes_every_field_as_base64url() {
        let credential = assertion_credential(raw_assertion()).unwrap();
        let json = serde_json::to_value(&credential).unwrap();
        assert_eq!(json["id"], json!(b64url(&[1, 2, 3, 4, 5])));
        assert_eq!(json["rawId"], json!(b64url(&[1, 2, 3, 4, 5])));
        assert_eq!(json["type"], json!("public-key"));
        assert_eq!(
            json["response"]["authenticatorData"],
            json!(b64url(&[0x49; 37]))
        );
        assert_eq!(
            json["response"]["signature"],
            json!(b64url(&[0x30, 0x45, 0xff]))
        );
        assert_eq!(json["response"]["userHandle"], json!("3q2-7w"));
        assert_eq!(
            json["response"]["clientDataJSON"],
            json!(b64url(b"{\"type\":\"webauthn.get\"}"))
        );

        let back: PublicKeyCredential = serde_json::from_value(json).unwrap();
        assert_eq!(
            back.response.user_handle.unwrap(),
            vec![0xde, 0xad, 0xbe, 0xef]
        );
    }

    #[test]
    fn assertion_credential_without_user_handle_sends_null() {
        let credential = assertion_credential(RawAssertion {
            user_handle: None,
            ..raw_assertion()
        })
        .unwrap();
        assert!(credential.response.user_handle.is_none());
        let json = serde_json::to_value(&credential).unwrap();
        assert_eq!(json["response"]["userHandle"], Value::Null);

        let credential = assertion_credential(RawAssertion {
            user_handle: Some(vec![]),
            ..raw_assertion()
        })
        .unwrap();
        assert!(credential.response.user_handle.is_none());
    }

    #[test]
    fn assertion_credential_rejects_empty_buffers() {
        for raw in [
            RawAssertion {
                raw_id: vec![],
                ..raw_assertion()
            },
            RawAssertion {
                client_data_json: vec![],
                ..raw_assertion()
            },
            RawAssertion {
                authenticator_data: vec![],
                ..raw_assertion()
            },
            RawAssertion {
                signature: vec![],
                ..raw_assertion()
            },
        ] {
            assert!(assertion_credential(raw).is_err());
        }
    }

    #[test]
    fn test_encoder_matches_known_vectors() {
        assert_eq!(b64url(b""), "");
        assert_eq!(b64url(b"f"), "Zg");
        assert_eq!(b64url(b"fo"), "Zm8");
        assert_eq!(b64url(b"foo"), "Zm9v");
        assert_eq!(b64url(&[0xfb, 0xff]), "-_8");
    }
}
