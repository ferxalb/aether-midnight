//! Cloud embedding tests against a deterministic loopback mock (see `tests/common/mock.rs`).
//! No test here talks to a real provider.

use super::*;
use std::time::Duration;

#[path = "../../tests/common/mock.rs"]
mod mock;
use mock::{Mock, Reply, fake_vector, ok_response};

const KEY: &str = "sk-test-SECRET-1234567890";
const DIMS: usize = 8;

fn config(provider: Provider, mock: &Mock) -> CloudConfig {
    CloudConfig::new(
        provider,
        Some("vendor/embed-small".into()),
        Some(DIMS),
        Some(mock.url.clone()),
        5,
        &[],
    )
    .unwrap()
}

fn embedder(provider: Provider, mock: &Mock) -> CloudEmbedder {
    CloudEmbedder::new(config(provider, mock), KEY.into(), "TEST_KEY").unwrap()
}

fn kind_of(error: anyhow::Error) -> CloudErrorKind {
    as_cloud_error(&error)
        .unwrap_or_else(|| panic!("not a CloudError: {error:#}"))
        .kind
}

fn norm(v: &[f32]) -> f32 {
    v.iter().map(|x| x * x).sum::<f32>().sqrt()
}

#[test]
fn config_requires_explicit_model_and_dimensions_and_rejects_unsupported_params() {
    let new = |model: Option<&str>, dims, params: &[&str]| {
        CloudConfig::new(
            Provider::OpenRouter,
            model.map(str::to_string),
            dims,
            None,
            30,
            &params.iter().map(|p| p.to_string()).collect::<Vec<_>>(),
        )
    };
    assert!(
        new(None, Some(8), &[])
            .unwrap_err()
            .to_string()
            .contains("--model")
    );
    assert!(
        new(Some("m"), None, &[])
            .unwrap_err()
            .to_string()
            .contains("--dimensions")
    );
    assert!(new(Some("m"), Some(0), &[]).is_err());
    assert!(new(Some("bad model"), Some(8), &[]).is_err());
    assert!(new(Some("m"), Some(8), &["dimensions=4"]).is_err());
    assert!(new(Some("m"), Some(8), &["encoding_format=base64"]).is_err());
    assert!(new(Some("m"), Some(8), &["user=a", "user=b"]).is_err());
    let ok = new(
        Some("m"),
        Some(8),
        &["input_type=search_query", "provider={\"order\":[\"x\"]}"],
    )
    .unwrap();
    assert_eq!(ok.endpoint.as_deref(), Some(OPENROUTER_ENDPOINT));
    assert_eq!(ok.params["input_type"], "search_query");
    assert_eq!(ok.params["provider"]["order"][0], "x");
    // `provider` routing is OpenRouter-only; Rainy accepts only `user`.
    assert!(
        CloudConfig::new(
            Provider::Rainy,
            Some("m".into()),
            Some(8),
            None,
            30,
            &["provider={}".into()]
        )
        .is_err()
    );
    assert!(CloudConfig::new(Provider::Local, None, None, None, 30, &[]).is_err());
    assert!(
        CloudConfig::new(
            Provider::Rainy,
            Some("m".into()),
            Some(8),
            Some("https://h/v1/embeddings".into()),
            30,
            &[]
        )
        .is_err()
    );
}

#[test]
fn identity_separates_provider_model_endpoint_params_and_dimensions() {
    let base = |p, m: &str, d, e: Option<&str>, params: &[&str]| {
        CloudConfig::new(
            p,
            Some(m.into()),
            Some(d),
            e.map(str::to_string),
            30,
            &params.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        )
        .unwrap()
        .identity()
    };
    let a = base(Provider::Rainy, "m", 8, Some("https://a.example/v1"), &[]);
    assert_eq!(
        a,
        base(Provider::Rainy, "m", 8, Some("https://a.example/v1/"), &[])
    );
    for other in [
        base(
            Provider::OpenRouter,
            "m",
            8,
            Some("https://a.example/v1"),
            &[],
        ),
        base(Provider::Rainy, "n", 8, Some("https://a.example/v1"), &[]),
        base(Provider::Rainy, "m", 16, Some("https://a.example/v1"), &[]),
        base(Provider::Rainy, "m", 8, Some("https://b.example/v1"), &[]),
        base(
            Provider::Rainy,
            "m",
            8,
            Some("https://a.example/v1"),
            &["user=u"],
        ),
    ] {
        assert_ne!(a, other);
    }
    assert!(a.name.starts_with("rainy:"));
    assert_ne!(a.name, crate::embed::MODEL_NAME);
}

#[test]
fn openrouter_request_format_auth_and_optional_params() {
    let mock = Mock::start(|_, r| ok_response(r, DIMS));
    let config = CloudConfig::new(
        Provider::OpenRouter,
        Some("openai/text-embedding-3-small".into()),
        Some(DIMS),
        Some(mock.url.clone()),
        5,
        &[
            "input_type=search_document".into(),
            "provider={\"allow_fallbacks\":false}".into(),
            "user=midnight".into(),
        ],
    )
    .unwrap();
    let mut e = CloudEmbedder::new(config, KEY.into(), OPENROUTER_KEY_ENV).unwrap();
    let out = e.embed(TextKind::Passage, &["alpha beta"]).unwrap();
    assert_eq!(out.len(), 1);
    let req = &mock.recorded()[0];
    assert_eq!(req.path, "/v1/embeddings");
    assert_eq!(req.headers["authorization"], format!("Bearer {KEY}"));
    assert!(req.headers["user-agent"].starts_with("aether-midnight/"));
    assert_eq!(req.body["model"], "openai/text-embedding-3-small");
    assert_eq!(req.body["input"], serde_json::json!(["alpha beta"]));
    assert_eq!(req.body["encoding_format"], "float");
    assert_eq!(req.body["dimensions"], DIMS);
    assert_eq!(req.body["input_type"], "search_document");
    assert_eq!(req.body["provider"]["allow_fallbacks"], false);
    assert_eq!(req.body["user"], "midnight");
    // No prefix contract remotely: text is sent verbatim.
    assert!(!req.body["input"][0].as_str().unwrap().contains("passage:"));
}

#[test]
fn rainy_custom_endpoint_uses_same_transport_and_omits_unset_optionals() {
    let mock = Mock::start(|_, r| ok_response(r, DIMS));
    let mut e = embedder(Provider::Rainy, &mock);
    e.embed(TextKind::Query, &["hello"]).unwrap();
    let req = &mock.recorded()[0];
    assert_eq!(req.path, "/v1/embeddings");
    assert_eq!(req.headers["authorization"], format!("Bearer {KEY}"));
    assert!(req.body.get("user").is_none() && req.body.get("provider").is_none());
    assert_eq!(e.identity().name, "rainy:vendor/embed-small");
    assert_eq!(e.identity().dimensions, DIMS);
}

#[test]
fn batches_of_sixteen_preserve_order_across_requests() {
    let mock = Mock::start(|_, r| ok_response(r, DIMS));
    let mut e = embedder(Provider::OpenRouter, &mock);
    let texts: Vec<String> = (0..40).map(|i| format!("doc{i} word{}", i * 7)).collect();
    let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
    let out = e.embed(TextKind::Passage, &refs).unwrap();
    assert_eq!(out.len(), 40);
    let sizes: Vec<usize> = mock
        .recorded()
        .iter()
        .map(|r| r.body["input"].as_array().unwrap().len())
        .collect();
    assert_eq!(sizes, vec![16, 16, 8]);
    for (text, vector) in texts.iter().zip(&out) {
        let mut expected = fake_vector(text, DIMS);
        l2_normalize(&mut expected);
        assert_eq!(vector, &expected, "vector for {text} out of order");
        assert!((norm(vector) - 1.0).abs() < 1e-5);
    }
}

#[test]
fn responses_are_placed_by_index_not_arrival_order() {
    let mock = Mock::start(|_, r| {
        let n = r.body["input"].as_array().unwrap().len();
        let mut data: Vec<_> = (0..n)
            .map(|i| {
                let text = r.body["input"][i].as_str().unwrap();
                serde_json::json!({"object":"embedding","index":i,"embedding":fake_vector(text, DIMS)})
            })
            .collect();
        data.reverse();
        Reply::Json(200, serde_json::json!({"object":"list","data":data,"model":"m","usage":{"prompt_tokens":1,"total_tokens":1}}).to_string())
    });
    let mut e = embedder(Provider::Rainy, &mock);
    let out = e
        .embed(TextKind::Passage, &["aaa", "bbb zzz", "ccc"])
        .unwrap();
    for (i, t) in ["aaa", "bbb zzz", "ccc"].iter().enumerate() {
        let mut expected = fake_vector(t, DIMS);
        l2_normalize(&mut expected);
        assert_eq!(out[i], expected);
    }
}

fn reply_with(body: serde_json::Value) -> Mock {
    Mock::start(move |_, _| Reply::Json(200, body.to_string()))
}

#[test]
fn invalid_dimensions_are_rejected_not_stored() {
    let mock = Mock::start(|_, r| ok_response(r, DIMS + 1));
    let mut e = embedder(Provider::OpenRouter, &mock);
    let err = e.embed(TextKind::Passage, &["x"]).unwrap_err();
    assert!(err.to_string().contains("expected 8"), "{err:#}");
    assert_eq!(kind_of(err), CloudErrorKind::Dimensions);
}

#[test]
fn malformed_vectors_and_payloads_are_rejected() {
    let good = vec![0.5f32; DIMS];
    let cases: Vec<(&str, serde_json::Value)> = vec![
        (
            "string element",
            serde_json::json!({"data":[{"object":"embedding","index":0,"embedding":["a","b"]}]}),
        ),
        (
            "null vector",
            serde_json::json!({"data":[{"object":"embedding","index":0,"embedding":null}]}),
        ),
        ("not json shape", serde_json::json!({"nope": true})),
        (
            "base64 although float requested",
            serde_json::json!({"data":[{"object":"embedding","index":0,"embedding":"AAAA"}]}),
        ),
        (
            "all zero",
            serde_json::json!({"data":[{"object":"embedding","index":0,"embedding":vec![0.0f32; DIMS]}]}),
        ),
        ("too few results", serde_json::json!({"data":[]})),
        (
            "too many results",
            serde_json::json!({"data":[
            {"object":"embedding","index":0,"embedding":good},
            {"object":"embedding","index":1,"embedding":good}]}),
        ),
        (
            "index out of range",
            serde_json::json!({"data":[{"object":"embedding","index":5,"embedding":good}]}),
        ),
    ];
    for (name, body) in cases {
        let mock = reply_with(body);
        let mut e = embedder(Provider::OpenRouter, &mock);
        let err = e.embed(TextKind::Passage, &["x"]).unwrap_err();
        assert_eq!(kind_of(err), CloudErrorKind::Malformed, "{name}");
    }
    // Duplicate index across two inputs.
    let mock = reply_with(serde_json::json!({"data":[
        {"object":"embedding","index":0,"embedding":good},
        {"object":"embedding","index":0,"embedding":good}]}));
    let mut e = embedder(Provider::OpenRouter, &mock);
    let err = e.embed(TextKind::Passage, &["a", "b"]).unwrap_err();
    assert_eq!(kind_of(err), CloudErrorKind::Malformed);
    // Non-HTTP garbage body (not JSON at all) with 200.
    let mock = Mock::start(|_, _| Reply::Status(200, "<html>gateway</html>"));
    let mut e = embedder(Provider::Rainy, &mock);
    assert_eq!(
        kind_of(e.embed(TextKind::Passage, &["x"]).unwrap_err()),
        CloudErrorKind::Malformed
    );
}

#[test]
fn request_timeout_is_reported_and_bounded() {
    let mock = Mock::start(|_, r| {
        Reply::Delayed(Duration::from_millis(2500), Box::new(ok_response(r, DIMS)))
    });
    let config = CloudConfig::new(
        Provider::Rainy,
        Some("m".into()),
        Some(DIMS),
        Some(mock.url.clone()),
        1,
        &[],
    )
    .unwrap();
    let mut e = CloudEmbedder::new(config, KEY.into(), "K").unwrap();
    let started = std::time::Instant::now();
    let err = e.embed(TextKind::Query, &["slow"]).unwrap_err();
    assert!(
        started.elapsed() < Duration::from_millis(2400),
        "timeout not enforced"
    );
    assert_eq!(kind_of(err), CloudErrorKind::Timeout);
    assert_eq!(mock.count(), 1, "no automatic retry");
}

#[test]
fn http_errors_are_classified_without_retry_and_never_leak_the_key() {
    let cases: Vec<(u16, String, CloudErrorKind)> = vec![
        (401, format!(r#"{{"error":{{"code":"invalid_api_key","message":"bad key {KEY}"}}}}"#), CloudErrorKind::Auth),
        (402, r#"{"error":{"code":402,"message":"Insufficient credits. Add more at example."}}"#.into(), CloudErrorKind::Payment),
        (402, r#"{"error":{"code":"INSUFFICIENT_CREDITS","message":"no credit","details":{"current_credits":0}}}"#.into(), CloudErrorKind::Payment),
        (429, r#"{"error":{"code":"RATE_LIMIT_EXCEEDED","message":"slow down","details":{"retry_after":7}}}"#.into(), CloudErrorKind::RateLimit),
        (429, "too many".into(), CloudErrorKind::RateLimit),
        (500, r#"{"error":{"message":"boom"}}"#.into(), CloudErrorKind::Server),
        (502, "bad gateway".into(), CloudErrorKind::Server),
        (503, r#"{"error":{"code":"PROVIDER_UNAVAILABLE","message":"down"}}"#.into(), CloudErrorKind::Server),
        (400, r#"{"error":{"message":"model not found"}}"#.into(), CloudErrorKind::Rejected),
    ];
    for (status, body, expected) in cases {
        let mock = {
            let body = body.clone();
            Mock::start(move |_, _| Reply::Json(status, body.clone()))
        };
        for provider in [Provider::OpenRouter, Provider::Rainy] {
            let before = mock.count();
            let mut e = embedder(provider, &mock);
            let err = e.embed(TextKind::Passage, &["x"]).unwrap_err();
            let rendered = format!("{err:#} / {err:?}");
            assert!(
                !rendered.contains(KEY),
                "secret leaked for HTTP {status}: {rendered}"
            );
            assert_eq!(kind_of(err), expected, "HTTP {status} body {body}");
            assert_eq!(mock.count() - before, 1, "HTTP {status} was retried");
        }
    }
}

#[test]
fn error_messages_are_bounded() {
    let huge = format!(r#"{{"error":{{"message":"{}"}}}}"#, "z".repeat(200_000));
    let mock = Mock::start(move |_, _| Reply::Json(500, huge.clone()));
    let mut e = embedder(Provider::OpenRouter, &mock);
    let err = e.embed(TextKind::Passage, &["x"]).unwrap_err();
    assert!(
        err.to_string().chars().count() < 400,
        "{}",
        err.to_string().len()
    );
}

#[test]
fn missing_credential_is_a_config_error_and_sends_nothing() {
    let mock = Mock::start(|_, r| ok_response(r, DIMS));
    let err = CloudEmbedder::new(
        config(Provider::OpenRouter, &mock),
        "  ".into(),
        "OPENROUTER_API_KEY",
    )
    .err()
    .unwrap();
    assert_eq!(kind_of(err), CloudErrorKind::Config);
    assert_eq!(mock.count(), 0);
}

#[test]
fn long_inputs_are_truncated_deterministically_and_empty_input_is_refused_locally() {
    let mock = Mock::start(|_, r| ok_response(r, DIMS));
    let mut e = embedder(Provider::Rainy, &mock);
    let long = "é".repeat(MAX_INPUT_CHARS + 500);
    e.embed(TextKind::Passage, &[&long]).unwrap();
    let sent = mock.recorded()[0].body["input"][0]
        .as_str()
        .unwrap()
        .chars()
        .count();
    assert_eq!(sent, MAX_INPUT_CHARS);
    let before = mock.count();
    assert!(e.embed(TextKind::Passage, &["  "]).is_err());
    assert_eq!(
        mock.count(),
        before,
        "empty input must not reach the network"
    );
}

#[test]
fn config_debug_output_contains_no_secret() {
    let mock = Mock::start(|_, r| ok_response(r, DIMS));
    let e = embedder(Provider::OpenRouter, &mock);
    assert!(!format!("{:?}", e.config()).contains(KEY));
}
