// See the project's Versioning rule: the package version must be semver, so an internal build is
// "0.1.29-dev.3" there and shows as "0.1.28.03" everywhere a person sees it.

/// The version people see: "0.1.28" for a release, "0.1.28.03" for internal build 3 after 0.1.28.
fn display_version(package: &str) -> String
{
    let Some((release, dev)) = package.split_once("-dev.") else { return package.to_string() };
    let parts: Vec<u32> = release.split('.').map(|part| part.parse().expect("version parts are numbers")).collect();
    let build: u32 = dev.parse().expect("the -dev part is a number");

    assert!(parts.len() == 3 && parts[2] > 0 && build > 0, "internal builds are A.B.C-dev.N with C and N above 0, not {package}");

    format!("{}.{}.{}.{:02}", parts[0], parts[1], parts[2] - 1, build)
}

/// Tauri's build step: bundles the window's files and icons. Also hands the app its shown version.
fn main()
{
    let package = std::env::var("CARGO_PKG_VERSION").unwrap();

    println!("cargo:rustc-env=SQS_DISPLAY_VERSION={}", display_version(&package));
    tauri_build::build()
}
