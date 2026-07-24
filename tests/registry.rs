//! Registry source, lockfile, and offline recipe load (Phase 7).

use isol8::recipe::RecipeRegistry;
use isol8::registry::{
    apply_update_to_lockfile, default_cache_root, diff_index, open_offline,
    parse_registries_from_toml, update_registry, DirSource, Lockfile, ProfileSource, RegistrySpec,
    TrustLevel,
};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/registry")
}

fn tmp_dir() -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "isol8-registry-itest-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn dir_source_fixture_has_sample_recipes() {
    let src = DirSource::open("fixture", fixture_root()).unwrap();
    assert_eq!(src.trust(), TrustLevel::Official);
    let ids = src.index().recipe_ids();
    assert!(ids.contains(&"toolchains/sample".into()));
    assert!(ids.contains(&"toolchains/sample-cache".into()));
}

#[test]
fn recipe_registry_loads_from_registry_dir() {
    let label = "registry:official:fixture".to_string();
    let recipes = fixture_root().join("recipes");
    let reg = RecipeRegistry::load_with_registry_dirs(&[], &[(label, recipes)]).unwrap();
    assert!(
        reg.ids().iter().any(|id| id == "toolchains/sample"),
        "expected sample from fixture, got {:?}",
        reg.ids()
    );
    // Builtin nvm still present.
    assert!(reg.ids().iter().any(|id| id == "toolchains/nvm"));
}

#[test]
fn path_update_writes_lockfile_and_opens_offline() {
    let dir = tmp_dir();
    let lock_path = dir.join("isol8.lock");
    let cache = dir.join("cache");
    let fixture = fixture_root().to_string_lossy().to_string();

    let spec = RegistrySpec::Path {
        path: fixture.clone(),
        trust: Some(TrustLevel::Official),
    };
    let upd = update_registry("fixture", &spec, &cache).unwrap();
    assert!(!upd.fetched);
    assert!(upd.entry_count >= 2);

    let src = DirSource::open_with_trust("fixture", &upd.path, Some(upd.trust)).unwrap();
    let mut lock = Lockfile::default();
    apply_update_to_lockfile(&mut lock, &upd, &src);
    lock.save(&lock_path).unwrap();

    let loaded = Lockfile::load(&lock_path).unwrap();
    assert!(loaded.registry("fixture").is_some());
    assert!(!loaded.entries.is_empty());

    let offline = open_offline("fixture", &spec, &cache, &loaded).unwrap();
    assert!(offline.get_recipe("toolchains/sample").unwrap().is_some());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn install_diff_flags_rw_real_home() {
    let src = DirSource::open("fixture", fixture_root()).unwrap();
    let items = diff_index(&Lockfile::default(), &src).unwrap();
    let cache = items
        .iter()
        .find(|i| i.id == "toolchains/sample-cache")
        .expect("sample-cache in diff");
    assert_eq!(cache.change, "added");
    assert!(
        cache.flags.iter().any(|f| f.contains("rw on real home")),
        "flags={:?}",
        cache.flags
    );
}

#[test]
fn parse_registries_toml() {
    let body = r#"
default_profiles = ["base"]

[registries.official]
path = "/tmp/isol8-recipes"
trust = "official"

[registries.work]
git = "https://example.com/recipes.git"
ref = "v1"
"#;
    let regs = parse_registries_from_toml(body).unwrap();
    assert_eq!(regs.len(), 2);
    assert!(matches!(
        regs.get("official").unwrap(),
        RegistrySpec::Path {
            trust: Some(TrustLevel::Official),
            ..
        }
    ));
    assert!(matches!(
        regs.get("work").unwrap(),
        RegistrySpec::Git { ref_name, .. } if ref_name == "v1"
    ));
}

#[test]
fn layered_source_later_wins_index() {
    use isol8::registry::LayeredSource;
    let a = DirSource::open("a", fixture_root()).unwrap();
    let b = DirSource::open("b", fixture_root()).unwrap();
    let layered = LayeredSource::new(vec![Box::new(a), Box::new(b)]);
    assert!(layered.index().get("toolchains/sample").is_some());
    let r = layered.get_recipe("toolchains/sample").unwrap().unwrap();
    // later source is b
    assert!(r.source.contains(":b:"), "source={}", r.source);
}

#[test]
fn default_cache_root_is_under_cache() {
    let root = default_cache_root();
    assert!(
        root.ends_with(Path::new("isol8/registries")) || root.to_string_lossy().contains("isol8"),
        "{root:?}"
    );
}
