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
fn batches_respect_item_cap_and_preserve_order_across_requests() {
    let mock = Mock::start(|_, r| ok_response(r, DIMS));
    let mut e = embedder(Provider::OpenRouter, &mock);
    let texts: Vec<String> = (0..40).map(|i| format!("doc{i} word{}", i * 7)).collect();
    let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
    let out = e.embed(TextKind::Passage, &refs).unwrap();
    assert_eq!(out.len(), 40);
    let mut sizes: Vec<usize> = mock
        .recorded()
        .iter()
        .map(|r| r.body["input"].as_array().unwrap().len())
        .collect();
    sizes.sort_unstable();
    assert_eq!(sizes, vec![8, 32]);
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
        (
            "overflowing normalization",
            serde_json::json!({"data":[{"index":0,"embedding":vec![3.0e38f32; DIMS]}]}),
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
fn http_errors_are_classified_retry_only_transient_and_never_leak_the_key() {
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
            e.retry_backoff = std::time::Duration::ZERO;
            let err = e.embed(TextKind::Passage, &["x"]).unwrap_err();
            let rendered = format!("{err:#} / {err:?}");
            assert!(
                !rendered.contains(KEY),
                "secret leaked for HTTP {status}: {rendered}"
            );
            assert_eq!(kind_of(err), expected, "HTTP {status} body {body}");
            let attempts = if matches!(expected, CloudErrorKind::RateLimit | CloudErrorKind::Server)
            {
                1 + REMOTE_RETRIES
            } else {
                1
            };
            assert_eq!(mock.count() - before, attempts, "HTTP {status} attempts");
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

#[test]
fn optional_metadata_and_indices_follow_wire_contract_without_weakening_ordering() {
    for provider in [Provider::OpenRouter, Provider::Rainy] {
        let mock = Mock::start(|_, r| {
            let Reply::Json(status, body) = ok_response(r, DIMS) else {
                unreachable!()
            };
            let mut value: Value = serde_json::from_str(&body).unwrap();
            value.as_object_mut().unwrap().remove("usage");
            value.as_object_mut().unwrap().remove("object");
            value.as_object_mut().unwrap().remove("model");
            for row in value["data"].as_array_mut().unwrap() {
                row.as_object_mut().unwrap().remove("index");
            }
            Reply::Json(status, value.to_string())
        });
        let mut e = embedder(provider, &mock);
        let got = e.embed(TextKind::Passage, &["alpha", "beta"]).unwrap();
        assert_eq!(got.len(), 2);
        for (text, actual) in ["alpha", "beta"].iter().zip(&got) {
            let mut expected = fake_vector(text, DIMS);
            l2_normalize(&mut expected);
            assert_eq!(*actual, expected);
        }
        let mixed = Mock::start(|_, _| {
            Reply::Json(
                200,
                serde_json::json!({"data":[
                    {"index":0,"embedding":vec![1.0; DIMS]},
                    {"embedding":vec![1.0; DIMS]}
                ]})
                .to_string(),
            )
        });
        assert_eq!(
            kind_of(
                embedder(provider, &mixed)
                    .embed(TextKind::Passage, &["a", "b"])
                    .unwrap_err()
            ),
            CloudErrorKind::Malformed
        );
    }
}

#[test]
fn payment_classification_uses_status_even_for_empty_or_misleading_bodies() {
    for provider in [Provider::OpenRouter, Provider::Rainy] {
        for (status, body, expected) in [
            (402, "", CloudErrorKind::Payment),
            (402, "unrelated", CloudErrorKind::Payment),
            (
                400,
                "billing payment credit balance",
                CloudErrorKind::Rejected,
            ),
        ] {
            let mock = Mock::start(move |_, _| Reply::Json(status, body.into()));
            assert_eq!(
                kind_of(
                    embedder(provider, &mock)
                        .embed(TextKind::Passage, &["a"])
                        .unwrap_err()
                ),
                expected
            );
        }
    }
}

#[test]
fn authenticated_redirects_are_rejected_without_forwarding_credentials() {
    let destination = Mock::start(|_, r| ok_response(r, DIMS));
    let location = format!("{}/embeddings", destination.url);
    let redirect = Mock::start(move |_, _| Reply::Redirect(location.clone()));
    for provider in [Provider::OpenRouter, Provider::Rainy] {
        assert_eq!(
            kind_of(
                embedder(provider, &redirect)
                    .embed(TextKind::Passage, &["private text"])
                    .unwrap_err()
            ),
            CloudErrorKind::Rejected
        );
    }
    assert_eq!(redirect.count(), 2);
    assert_eq!(destination.count(), 0);
}

#[test]
fn transient_failure_is_retried_and_order_is_kept() {
    // The first request of every batch gets a 429; the retry succeeds.
    let seen = std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashSet::new()));
    let mock = Mock::start(move |_, r| {
        let key = r.body["input"].to_string();
        if seen.lock().unwrap().insert(key) {
            Reply::Json(429, "slow".into())
        } else {
            ok_response(r, DIMS)
        }
    });
    let mut e = embedder(Provider::OpenRouter, &mock);
    e.retry_backoff = std::time::Duration::ZERO;
    let texts: Vec<String> = (0..70).map(|i| format!("doc{i} word{}", i * 3)).collect();
    let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
    let out = e.embed(TextKind::Passage, &refs).unwrap();
    assert_eq!(out.len(), 70);
    for (text, vector) in texts.iter().zip(&out) {
        let mut expected = fake_vector(text, DIMS);
        l2_normalize(&mut expected);
        assert_eq!(vector, &expected, "vector for {text} out of order");
    }
    assert_eq!(mock.count(), 6, "3 batches (32+32+6), each tried twice");
}

#[test]
fn one_failed_batch_aborts_the_call_and_stops_new_requests() {
    // Auth failure is not retried; remaining queued batches must not all be sent.
    let mock = Mock::start(|_, _| Reply::Json(401, "no".into()));
    let mut e = embedder(Provider::OpenRouter, &mock);
    e.concurrency = 2;
    let texts: Vec<String> = (0..REMOTE_BATCH_MAX_ITEMS * 40)
        .map(|i| format!("doc{i}"))
        .collect();
    let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
    let err = e.embed(TextKind::Passage, &refs).unwrap_err();
    assert_eq!(kind_of(err), CloudErrorKind::Auth);
    assert!(
        mock.count() < 40,
        "sent {} of 40 batches after a failure",
        mock.count()
    );
}

fn strings(sizes: &[usize]) -> Vec<String> {
    sizes.iter().map(|n| "x".repeat(*n)).collect()
}

#[test]
fn plan_batches_caps_items_and_bytes_and_covers_every_input_in_order() {
    let many = strings(&[10; 70]);
    let sizes: Vec<usize> = plan_batches(&many).iter().map(|b| b.len()).collect();
    assert_eq!(sizes, vec![32, 32, 6]);

    // 8 KB each: 64_000 / 8_000 = 8 per request, never more bytes than the cap.
    let big = strings(&[8_000; 20]);
    let planned = plan_batches(&big);
    assert_eq!(
        planned.iter().map(|b| b.len()).collect::<Vec<_>>(),
        vec![8, 8, 4]
    );
    for batch in &planned {
        assert!(batch.iter().map(String::len).sum::<usize>() <= REMOTE_BATCH_MAX_BYTES);
    }

    // A text that alone reaches the cap still gets a request of its own, never an empty one.
    let mixed = strings(&[60_000, 10_000, 1]);
    let planned = plan_batches(&mixed);
    assert_eq!(
        planned.iter().map(|b| b.len()).collect::<Vec<_>>(),
        vec![1, 2]
    );
    let rejoined: Vec<&String> = planned.iter().flat_map(|b| b.iter()).collect();
    assert_eq!(rejoined, mixed.iter().collect::<Vec<_>>());
    assert!(plan_batches(&[]).is_empty());
}

#[test]
fn failure_classification_follows_the_openrouter_error_guide() {
    let secs = Duration::from_secs;
    let hint = |status, header, body| classify_failure(status, header, body);
    for status in [408u16, 500, 502, 503, 529] {
        let h = hint(status, None, "");
        assert!(h.retryable && !h.rate_limited, "HTTP {status}");
    }
    let h = hint(
        429,
        None,
        "You exceeded your tokens quota. Retry the request after 2 sec.",
    );
    assert!(h.retryable && h.rate_limited);
    assert_eq!(h.delay, Some(secs(2)));
    // The header wins over the message.
    assert_eq!(
        hint(429, Some(secs(7)), "retry after 2 sec").delay,
        Some(secs(7))
    );
    // 402 is retried only for the in-flight budget and only when Retry-After is present.
    let budget = r#"{"error":{"metadata":{"limit_source":"openrouter_in_flight_budget"}}}"#;
    assert!(hint(402, Some(secs(3)), budget).retryable);
    assert!(!hint(402, None, budget).retryable);
    assert!(!hint(402, Some(secs(3)), "Insufficient credits").retryable);
    for status in [400, 401, 403, 404, 413] {
        assert_eq!(
            hint(status, Some(secs(1)), "retry after 1 sec"),
            RetryHint::default()
        );
    }
}

#[test]
fn retry_hint_in_message_is_parsed_strictly() {
    let secs = Duration::from_secs;
    assert_eq!(
        delay_from_message("Retry the request after 2 sec."),
        Some(secs(2))
    );
    assert_eq!(delay_from_message("RETRY AFTER 15 seconds"), Some(secs(15)));
    assert_eq!(delay_from_message("retry after 0 sec"), Some(secs(0)));
    assert_eq!(delay_from_message("after a while"), None);
    assert_eq!(delay_from_message("after 5 minutes"), None);
    assert_eq!(delay_from_message("no hint here"), None);
    assert_eq!(delay_from_message(""), None);
}

#[test]
fn retry_delay_uses_hint_else_exponential_backoff_with_bounded_jitter() {
    let base = Duration::from_secs(1);
    let none = RetryHint {
        retryable: true,
        ..RetryHint::default()
    };
    for (attempt, floor) in [(1, 1), (2, 2), (3, 4)] {
        let d = retry_delay(&none, base, attempt);
        assert!(
            d >= Duration::from_secs(floor)
                && d < Duration::from_secs(floor) + Duration::from_millis(250)
        );
    }
    let hinted = RetryHint {
        delay: Some(Duration::from_secs(5)),
        ..none.clone()
    };
    let d = retry_delay(&hinted, base, 1);
    assert!(d >= Duration::from_secs(5) && d < Duration::from_millis(5250));
    let huge = RetryHint {
        delay: Some(Duration::from_secs(3600)),
        ..none
    };
    assert_eq!(retry_delay(&huge, base, 1), MAX_RETRY_DELAY);
}

#[test]
fn gate_halves_once_per_burst_and_regrows_one_slot_per_window_of_successes() {
    let gate = Gate::new(8);
    let abort = AtomicBool::new(false);
    assert!(gate.acquire(&abort));
    gate.release();
    gate.on_rate_limit();
    gate.on_rate_limit(); // same burst: counts once
    assert_eq!(gate.state.lock().unwrap().allowed, 4);
    for _ in 0..3 {
        gate.on_success();
    }
    assert_eq!(gate.state.lock().unwrap().allowed, 4, "needs 4 successes");
    gate.on_success();
    assert_eq!(gate.state.lock().unwrap().allowed, 5);
    for _ in 0..200 {
        gate.on_success();
    }
    assert_eq!(
        gate.state.lock().unwrap().allowed,
        8,
        "never above the ceiling"
    );
    let one = Gate::new(1);
    one.on_rate_limit();
    assert_eq!(one.state.lock().unwrap().allowed, 1, "never below one");
}

#[test]
fn gate_blocks_at_the_limit_and_stops_waiters_on_abort() {
    let gate = Gate::new(1);
    let abort = AtomicBool::new(false);
    assert!(gate.acquire(&abort));
    std::thread::scope(|scope| {
        let waiter = scope.spawn(|| gate.acquire(&abort));
        std::thread::sleep(Duration::from_millis(100));
        abort.store(true, Ordering::Relaxed);
        assert!(!waiter.join().unwrap(), "waiter must give up after abort");
    });
}

#[test]
fn http_408_is_retried_and_reported_as_timeout_when_it_persists() {
    let mock = Mock::start(|n, r| {
        if n == 0 {
            Reply::Json(408, "".into())
        } else {
            ok_response(r, DIMS)
        }
    });
    let mut e = embedder(Provider::OpenRouter, &mock);
    e.retry_backoff = Duration::ZERO;
    assert_eq!(e.embed(TextKind::Passage, &["a b"]).unwrap().len(), 1);
    assert_eq!(mock.count(), 2);

    let always = Mock::start(|_, _| Reply::Json(408, "".into()));
    let mut e = embedder(Provider::OpenRouter, &always);
    e.retry_backoff = Duration::ZERO;
    let err = e.embed(TextKind::Passage, &["a b"]).unwrap_err();
    assert_eq!(kind_of(err), CloudErrorKind::Timeout);
    assert_eq!(always.count(), 1 + REMOTE_RETRIES);
}

#[test]
fn payment_required_is_not_retried_without_the_in_flight_budget_marker() {
    let mock = Mock::start(|_, _| {
        Reply::Json(
            402,
            r#"{"error":{"message":"Insufficient credits"}}"#.into(),
        )
    });
    let mut e = embedder(Provider::OpenRouter, &mock);
    e.retry_backoff = Duration::ZERO;
    let err = e.embed(TextKind::Passage, &["a b"]).unwrap_err();
    assert_eq!(kind_of(err), CloudErrorKind::Payment);
    assert_eq!(mock.count(), 1);
}

#[test]
fn sustained_rate_limiting_still_returns_every_vector_in_order() {
    // Every first attempt per batch is rate limited with a provider hint; concurrency adapts.
    let seen = std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashSet::new()));
    let mock = Mock::start(move |_, r| {
        if seen.lock().unwrap().insert(r.body["input"].to_string()) {
            Reply::Json(429, "Retry the request after 0 sec".into())
        } else {
            ok_response(r, DIMS)
        }
    });
    let mut e = embedder(Provider::OpenRouter, &mock);
    e.retry_backoff = Duration::ZERO;
    e.concurrency = 8;
    let texts: Vec<String> = (0..32 * 12).map(|i| format!("doc{i} w{}", i * 5)).collect();
    let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
    let out = e.embed(TextKind::Passage, &refs).unwrap();
    assert_eq!(out.len(), texts.len());
    for (text, vector) in texts.iter().zip(&out) {
        let mut expected = fake_vector(text, DIMS);
        l2_normalize(&mut expected);
        assert_eq!(vector, &expected, "vector for {text} out of order");
    }
    assert_eq!(
        mock.count(),
        24,
        "12 batches, each rate limited once then served"
    );
}

#[test]
fn interactive_query_retries_timeout_once_and_reports_the_error_class() {
    let mock = Mock::start(|_, request| {
        Reply::Delayed(
            Duration::from_millis(1200),
            Box::new(ok_response(request, DIMS)),
        )
    });
    let mut client = embedder(Provider::OpenRouter, &mock)
        .interactive_queries(500)
        .unwrap();
    client.retry_backoff = Duration::ZERO;
    let error = client.embed(TextKind::Query, &["slow query"]).unwrap_err();
    assert!(error.to_string().starts_with("timeout:"), "{error:#}");
    assert_eq!(kind_of(error), CloudErrorKind::Timeout);
    assert_eq!(mock.count(), 2);
}
