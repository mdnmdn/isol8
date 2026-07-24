//! Phase 5: shared denial analysis (NDJSON feed → recipe suggestions).

use isol8::analyze::{self, Denial, DenialAccess};
use isol8::context::{Context, Platform};
use isol8::filter::RunContext;
use isol8::recipe::RecipeRegistry;
use std::path::{Path, PathBuf};

#[test]
fn end_to_end_feed_to_report() {
    let body = r#"
{"path":"/Users/alice/.m2/repository/org/foo/1.0/foo.jar","access":"read","count":50}
{"path":"/Users/alice/.m2/repository/org/bar/2.0/bar.jar","access":"write","count":3}
{"path":"/Users/alice/.nvm/versions/node/v22.0.0/bin/node","access":"read","count":12}
{"path":"/Users/alice/.ssh/id_rsa","access":"read","count":1}
"#;
    let denials = analyze::parse_ndjson(body).unwrap();
    let reg = RecipeRegistry::load(&[]).unwrap();
    let ctx = RunContext {
        cmd: vec![],
        os: "macos".into(),
        arch: "aarch64".into(),
    };
    let ambient = Context {
        real_home: PathBuf::from("/Users/alice"),
        cwd: PathBuf::from("/tmp"),
        platform: Platform::Macos,
        managed_root: PathBuf::from("/Users/alice/.local/share/isol8/homes"),
    };
    let eff = Path::new("/tmp/scratch-home");
    let index = analyze::build_recipe_index(&reg, &ctx, &ambient, eff).unwrap();
    let report = analyze::analyze(&denials, &index, &ambient, eff, "test feed");
    let text = report.render();
    assert!(text.contains("toolchains/maven"), "{text}");
    assert!(text.contains("toolchains/nvm"), "{text}");
    assert!(text.contains(".ssh") || text.contains("no match"), "{text}");
    assert!(report.total_denials >= 66);
}

#[test]
fn denial_access_parse() {
    assert_eq!(DenialAccess::parse("file-read-data"), DenialAccess::Read);
    assert_eq!(DenialAccess::parse("rw"), DenialAccess::Write);
    let d = Denial {
        path: PathBuf::from("/x"),
        access: DenialAccess::Read,
        count: 1,
        pid: 0,
        exe: None,
    };
    assert_eq!(d.count, 1);
}
