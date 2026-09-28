//! Check GitHub Releases for a newer version to install manually.
use semver::Version;
use serde::Deserialize;
use std::time::Duration;

const API: &str = "https://api.github.com/repos/toorux/steam-frame-6ghz-tool/releases?per_page=100";
const ASSET: &str = "steam-frame-6ghz-tool.exe";
const RELEASE_TAGS: &str = "https://github.com/toorux/steam-frame-6ghz-tool/releases/tag/";

#[derive(Deserialize)]
struct ApiAsset {
    name: String,
}

#[derive(Deserialize)]
struct ApiRelease {
    tag_name: String,
    prerelease: bool,
    draft: bool,
    html_url: String,
    assets: Vec<ApiAsset>,
}

#[derive(Clone, Debug)]
pub struct Release {
    pub version: Version,
    pub page: String,
}

fn choose(releases: Vec<ApiRelease>, current: &Version) -> Option<Release> {
    releases
        .into_iter()
        .filter_map(|release| {
            if release.draft || (current.pre.is_empty() && release.prerelease) {
                return None;
            }
            let version = Version::parse(release.tag_name.strip_prefix('v')?).ok()?;
            if version <= *current || release.prerelease == version.pre.is_empty() {
                return None;
            }
            if release.html_url != format!("{RELEASE_TAGS}{}", release.tag_name)
                || !release.assets.iter().any(|asset| asset.name == ASSET)
            {
                return None;
            }
            Some(Release {
                version,
                page: release.html_url,
            })
        })
        .max_by(|a, b| a.version.cmp(&b.version))
}

pub fn check() -> Result<Option<Release>, String> {
    let current = Version::parse(env!("CARGO_PKG_VERSION")).map_err(|e| e.to_string())?;
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(25)))
        .build()
        .into();
    let mut response = agent
        .get(API)
        .header("User-Agent", "steam-frame-6ghz-tool")
        .header("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| e.to_string())?;
    let releases: Vec<ApiRelease> = response.body_mut().read_json().map_err(|e| e.to_string())?;
    Ok(choose(releases, &current))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(tag: &str, prerelease: bool) -> ApiRelease {
        ApiRelease {
            tag_name: tag.into(),
            prerelease,
            draft: false,
            html_url: format!("{RELEASE_TAGS}{tag}"),
            assets: vec![ApiAsset { name: ASSET.into() }],
        }
    }

    #[test]
    fn selects_newer_release_for_current_channel() {
        let list = || vec![release("v0.1.0-preview.4", true), release("v0.1.0", false)];
        assert_eq!(
            choose(list(), &Version::parse("0.1.0-preview.3").unwrap())
                .unwrap()
                .version
                .to_string(),
            "0.1.0"
        );
        assert!(choose(list(), &Version::parse("0.1.0").unwrap()).is_none());
    }

    #[test]
    fn ignores_untrusted_page_and_release_without_program() {
        let mut wrong_page = release("v0.1.0-preview.5", true);
        wrong_page.html_url = "https://example.com/release".into();
        let mut no_program = release("v0.1.0-preview.6", true);
        no_program.assets.clear();
        assert!(
            choose(
                vec![wrong_page, no_program],
                &Version::parse("0.1.0-preview.4").unwrap()
            )
            .is_none()
        );
    }

    #[test]
    #[ignore = "Read-only GitHub Release API check"]
    fn github_release_api_smoke() {
        println!("available={:?}", check().unwrap());
    }
}
