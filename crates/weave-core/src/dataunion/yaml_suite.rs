use super::{certify, load, undeclared, Format};
use serde::Deserialize;
use serde_json::Value;
use std::{env, fs, path::Path, process::Command};

#[derive(Deserialize)]
struct RequiredCase {
    name: String,
    input: String,
    #[serde(default)]
    reject: bool,
    json: Option<Vec<Value>>,
    equivalent: Option<String>,
    changed: Option<String>,
}

#[test]
fn required_yaml_behavior() {
    let cases: Vec<RequiredCase> =
        serde_json::from_str(include_str!("../../tests/fixtures/yaml/required.json")).unwrap();
    assert!(!cases.is_empty());
    for case in cases {
        let input = &case.input;
        let keys = crate::datafile::keys("input.yaml", input).unwrap();
        assert_eq!(keys.is_err(), case.reject, "{}: {keys:?}", case.name);
        assert_eq!(load(input, Format::Yaml), case.json, "{}", case.name);
        if let Some(equivalent) = case.equivalent {
            let docs = |text| {
                crate::datafile::yaml_documents(text)
                    .collect::<Result<Vec<_>, _>>()
                    .unwrap()
            };
            assert_eq!(docs(input), docs(&equivalent), "{}", case.name);
        }
        if let Some(changed) = case.changed {
            assert!(
                !certify(input, input, input, &changed, "input.yaml", &undeclared),
                "{}: certified a changed value",
                case.name
            );
        }
    }
}

#[test]
#[ignore = "requires the yaml-test-suite data checkout used in CI"]
fn upstream_yaml_fixtures() {
    let root = env::var_os("YAML_TEST_SUITE").expect("set YAML_TEST_SUITE to the fixture checkout");
    let root = Path::new(&root);
    let files = Command::new("git")
        .current_dir(root)
        .args(["ls-files", "--", "*/in.yaml"])
        .output()
        .unwrap();
    assert!(files.status.success(), "could not list YAML fixtures");
    let files = String::from_utf8(files.stdout).unwrap();
    let mut compared = 0;
    for file in files.lines() {
        let id = file.strip_suffix("/in.yaml").unwrap();
        let dir = root.join(id);
        let yaml = fs::read_to_string(root.join(file)).unwrap();
        let keys = crate::datafile::keys("input.yaml", &yaml).unwrap();
        let values = load(&yaml, Format::Yaml);
        if dir.join("error").exists() {
            assert!(
                keys.is_err() && values.is_none(),
                "{id}: expected invalid YAML"
            );
            continue;
        }
        // The event parser accepts these, but their mappings repeat a key.
        if matches!(id, "2JQS" | "X38W") {
            assert!(keys.unwrap_err().contains("duplicate key"), "{id}");
            assert!(values.is_none(), "{id}");
            continue;
        }
        assert!(keys.is_ok(), "{id}: {keys:?}");
        let json = dir.join("in.json");
        if json.exists() {
            compared += 1;
            // Upstream's JSON omits these YAML tags. A merge must preserve them.
            let tagged = [
                "2XXW", "565N", "5TYM", "6CK3", "6WLZ", "7FWL", "9WXW", "C4HZ", "CC74", "CUP7",
                "J7PZ", "M5C3", "P76L", "UGM3", "Z67P", "Z9M4",
            ];
            if tagged.contains(&id) {
                assert!(values.is_none(), "{id}: tag lost in JSON conversion");
            } else {
                let json = fs::read_to_string(json).unwrap();
                let expected = serde_json::Deserializer::from_str(&json)
                    .into_iter::<Value>()
                    .collect::<Result<Vec<_>, _>>()
                    .unwrap();
                assert_eq!(values.as_ref(), Some(&expected), "{id}");
            }
        }
    }
    assert_eq!((files.lines().count(), compared), (402, 279));
}
