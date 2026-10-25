use std::path::Path;
use std::process::Command;

const DOMAIN: &str = "beam";
const LOCALES: &[(&str, &str)] = &[
    ("en-US", "en_US"),
    ("pt-BR", "pt_BR"),
    ("es-ES", "es_ES"),
    ("zh-CN", "zh_CN"),
];

fn main() {
    println!("cargo:rerun-if-changed=po");
    let po_dir = Path::new("po");
    assert!(
        Command::new("msgfmt").arg("--version").output().is_ok(),
        "msgfmt is required to build Beam translation catalogs"
    );

    for (catalog, locale_dir) in LOCALES {
        let source = po_dir.join(format!("{catalog}.po"));
        assert!(
            source.is_file(),
            "missing required catalog: {}",
            source.display()
        );
        let output_dir = po_dir.join("locale").join(locale_dir).join("LC_MESSAGES");
        std::fs::create_dir_all(&output_dir).expect("create local catalog directory");
        let status = Command::new("msgfmt")
            .arg("--check")
            .arg("--check-format")
            .arg("-o")
            .arg(output_dir.join(format!("{DOMAIN}.mo")))
            .arg(&source)
            .status()
            .expect("run msgfmt");
        assert!(status.success(), "failed to compile {}", source.display());
    }
    tauri_build::build();
}
