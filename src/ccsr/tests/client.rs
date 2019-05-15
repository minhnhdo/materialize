// Copyright 2019 Materialize, Inc. All rights reserved.
//
// This file is part of Materialize. Materialize may not be used or
// distributed without the express permission of Materialize, Inc.

use std::env;
use std::sync::Mutex;

use hyper::rt::Future;
use hyper::server::conn::AddrIncoming;
use hyper::service;
use hyper::Server;
use hyper::StatusCode;
use hyper::{Body, Response};
use lazy_static::lazy_static;
use tokio::runtime::Runtime;

use ccsr::{Client, DeleteError, GetError, PublishError};

lazy_static! {
    pub static ref SCHEMA_REGISTRY_URL: reqwest::Url = match env::var("SCHEMA_REGISTRY_URL") {
        Ok(addr) => addr.parse().expect("unable to parse SCHEMA_REGISTRY_URL"),
        _ => "http://localhost:8081".parse().unwrap(),
    };
}

#[test]
fn test_client() -> Result<(), failure::Error> {
    let client = Client::new(SCHEMA_REGISTRY_URL.clone());

    let existing_subjects = client.list_subjects()?;
    for s in existing_subjects {
        if s.starts_with("ccsr-test-") {
            client.delete_subject(&s)?;
        }
    }

    let schema_v1 = r#"{ "type": "record", "name": "na", "fields": [
        { "name": "a", "type": "long" }
    ]}"#;

    let schema_v2 = r#"{ "type": "record", "name": "na", "fields": [
        { "name": "a", "type": "long" },
        { "name": "b", "type": "long", "default": 0 }
    ]}"#;

    let schema_v2_incompat = r#"{ "type": "record", "name": "na", "fields": [
        { "name": "a", "type": "string" }
    ]}"#;

    assert_eq!(count_schemas(&client, "ccsr-test-")?, 0);

    let schema_v1_id = client.publish_schema("ccsr-test-schema", schema_v1)?;
    assert!(schema_v1_id > 0);

    match client.publish_schema("ccsr-test-schema", schema_v2_incompat) {
        Err(PublishError::IncompatibleSchema) => (),
        res => panic!("expected IncompatibleSchema error, got {:?}", res),
    }

    let schema_v2_id = client.publish_schema("ccsr-test-schema", schema_v2)?;
    assert!(schema_v2_id > 0);
    assert!(schema_v2_id > schema_v1_id);

    assert_eq!(
        schema_v1_id,
        client.publish_schema("ccsr-test-schema", schema_v1)?
    );

    assert_schemas_eq(schema_v1, &client.get_schema(schema_v1_id)?);
    assert_schemas_eq(schema_v2, &client.get_schema(schema_v2_id)?);

    assert_eq!(count_schemas(&client, "ccsr-test-")?, 1);

    client.publish_schema("ccsr-test-another-schema", "\"int\"")?;
    assert_eq!(count_schemas(&client, "ccsr-test-")?, 2);

    Ok(())
}

#[test]
fn test_client_errors() -> Result<(), failure::Error> {
    let client = Client::new(SCHEMA_REGISTRY_URL.clone());

    // Get-specific errors.
    match client.get_schema(i32::max_value()) {
        Err(GetError::SchemaNotFound) => (),
        res => panic!("expected GetError::SchemaNotFound, got {:?}", res),
    }

    // Publish-specific errors.
    match client.publish_schema("ccsr-test-schema", "blah") {
        Err(PublishError::InvalidSchema) => (),
        res => panic!("expected PublishError::InvalidSchema, got {:?}", res),
    }

    // Delete-specific errors.
    match client.delete_subject("ccsr-test-noexist") {
        Err(DeleteError::SubjectNotFound) => (),
        res => panic!("expected DeleteError::SubjectNotFound, got {:?}", res),
    }

    Ok(())
}

#[test]
fn test_server_errors() -> Result<(), failure::Error> {
    // When the schema registry gracefully reports an error by including a
    // properly-formatted JSON document in the response, the specific error code
    // and message should be propagated.

    let client_graceful = start_server(
        StatusCode::INTERNAL_SERVER_ERROR,
        r#"{ "error_code": 50001, "message": "overloaded; try again later" }"#,
    );

    match client_graceful.publish_schema("foo", "bar") {
        Err(PublishError::Server {
            code: 50001,
            ref message,
        }) if message == "overloaded; try again later" => (),
        res => panic!("expected PublishError::Server, got {:?}", res),
    }

    match client_graceful.get_schema(0) {
        Err(GetError::Server {
            code: 50001,
            ref message,
        }) if message == "overloaded; try again later" => (),
        res => panic!("expected GetError::Server, got {:?}", res),
    }

    match client_graceful.delete_subject("foo") {
        Err(DeleteError::Server {
            code: 50001,
            ref message,
        }) if message == "overloaded; try again later" => (),
        res => panic!("expected DeleteError::Server, got {:?}", res),
    }

    // If the schema registry crashes so hard that it spits out an exception
    // handler in the response, we should report the HTTP status code and a
    // generic message indicating that no further details were available.

    let client_crash = start_server(
        StatusCode::INTERNAL_SERVER_ERROR,
        r#"panic! an exception occured!"#,
    );

    match client_crash.publish_schema("foo", "bar") {
        Err(PublishError::Server {
            code: 500,
            ref message,
        }) if message == "unable to decode error details" => (),
        res => panic!("expected PublishError::Server, got {:?}", res),
    }

    match client_crash.get_schema(0) {
        Err(GetError::Server {
            code: 500,
            ref message,
        }) if message == "unable to decode error details" => (),
        res => panic!("expected GetError::Server, got {:?}", res),
    }

    match client_crash.delete_subject("foo") {
        Err(DeleteError::Server {
            code: 500,
            ref message,
        }) if message == "unable to decode error details" => (),
        res => panic!("expected DeleteError::Server, got {:?}", res),
    }

    Ok(())
}

fn start_server(status_code: StatusCode, body: &'static str) -> Client {
    let incoming = AddrIncoming::bind(&([127, 0, 0, 1], 0).into()).unwrap();
    let addr = incoming.local_addr();
    let server = Server::builder(incoming).serve(move || {
        service::service_fn_ok(move |_req| {
            Response::builder()
                .status(status_code)
                .body(Body::from(body))
                .unwrap()
        })
    });

    RUNTIME.lock().unwrap().spawn(server.map_err(|e| {
        eprintln!("server error: {}", e);
    }));

    let url: reqwest::Url = format!("http://{}", addr).parse().unwrap();
    Client::new(url)
}

lazy_static! {
    static ref RUNTIME: Mutex<Runtime> = Mutex::new(Runtime::new().unwrap());
}

fn assert_schemas_eq(schema1: &str, schema2: &str) {
    let schema1: serde_json::Value = serde_json::from_str(schema1).unwrap();
    let schema2: serde_json::Value = serde_json::from_str(schema2).unwrap();
    assert_eq!(schema1, schema2);
}

fn count_schemas(client: &Client, subject_prefix: &str) -> Result<usize, failure::Error> {
    Ok(client
        .list_subjects()?
        .iter()
        .filter(|s| s.starts_with(subject_prefix))
        .count())
}