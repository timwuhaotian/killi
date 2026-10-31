use super::super::with_provider_fields;

fn fields(pairs: &[(&str, toml::Value)]) -> toml::Table {
    pairs
        .iter()
        .map(|(field, value)| (field.to_string(), value.clone()))
        .collect()
}

fn mapping(pairs: &[(&str, &str)]) -> toml::Value {
    toml::Value::Table(
        pairs
            .iter()
            .map(|(from, to)| (from.to_string(), toml::Value::from(*to)))
            .collect(),
    )
}

#[test]
fn a_provider_gets_the_fields_in_place_and_keeps_its_neighbours() {
    let text = "# top\n[providers.p]  # mine\ntype = \"openai\"\ntool_names.bash = \"old\"\ntool_names.keep = \"mine\"\napi_key_env = \"P_KEY\"\n\n# next\n[providers.q]\ntype = \"openai\"\napi_key = \"q\"\n";
    let given = fields(&[
        ("type", "openai_responses".into()),
        ("tool_names", mapping(&[("bash", "run_command")])),
        ("api_key", "new".into()),
    ]);
    let (out, changed) = with_provider_fields(text, "p", &given).unwrap();
    assert_eq!(changed, ["api_key", "tool_names", "type"]);
    assert_eq!(
        out,
        "# top\n[providers.p]  # mine\ntype = \"openai_responses\"\ntool_names = { bash = \"run_command\", keep = \"mine\" }\napi_key = \"new\"\n\n# next\n[providers.q]\ntype = \"openai\"\napi_key = \"q\"\n",
        "each field takes the place of the lines that set it"
    );
    let (again, none) = with_provider_fields(&out, "p", &given).unwrap();
    assert!(again == out && none.is_empty(), "{again}");
}

#[test]
fn a_new_field_goes_under_the_header_and_a_key_pair_collapses() {
    let text = "[providers.p]\ntype = \"openai\"\napi_key = \"same\"\napi_key_env = \"P_KEY\"\n";
    let given = fields(&[
        ("api_key", "same".into()),
        ("session_header", "x-session".into()),
    ]);
    let (out, changed) = with_provider_fields(text, "p", &given).unwrap();
    assert_eq!(changed, ["api_key", "session_header"]);
    assert_eq!(
        out,
        "[providers.p]\nsession_header = \"x-session\"\ntype = \"openai\"\napi_key = \"same\"\n",
        "only one key line stays, as only one is read"
    );
}

#[test]
fn a_provider_without_its_own_table_is_refused() {
    let text = "providers.p.type = \"openai\"\n";
    let error = with_provider_fields(text, "p", &fields(&[("type", "anthropic".into())]))
        .unwrap_err()
        .to_string();
    assert!(error.contains("no [providers.p] table"), "{error}");
}
