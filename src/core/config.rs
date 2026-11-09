use std::path::{Path, PathBuf};

/// Video filename candidates for 1-based screen number `n`.
///
/// `wallpaper{n}.mp4` -> `wallpaper1.mp4` (n > 1) -> `wallpaper.mp4`.
pub fn candidate_names(n: usize) -> Vec<String> {
    assert!(n >= 1, "screen numbers are 1-based");
    let mut names = Vec::new();
    let push = |names: &mut Vec<String>, name: String| {
        if !names.contains(&name) {
            names.push(name);
        }
    };
    push(&mut names, format!("wallpaper{n}.mp4"));
    push(&mut names, "wallpaper1.mp4".to_string());
    push(&mut names, "wallpaper.mp4".to_string());
    names
}

/// Pick the first candidate accepted by `exists`; fall back to the first name.
pub fn choose_video(n: usize, exists: impl Fn(&str) -> bool) -> String {
    let names = candidate_names(n);
    for name in &names {
        if exists(name) {
            return name.clone();
        }
    }
    names.into_iter().next().unwrap()
}

/// Absolute path of the chosen video for screen `n`, resolved against `dir`.
pub fn resolve_video_for(n: usize, dir: &Path) -> PathBuf {
    let names = candidate_names(n);
    for name in &names {
        let candidate = dir.join(name);
        if candidate.exists() {
            return candidate;
        }
    }
    dir.join(&names[0])
}

/// Directory of the running executable, falling back to the current dir.
pub fn exe_dir_or_cwd() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}

#[cfg(test)]
mod tests {
    use super::{candidate_names, choose_video};

    #[test]
    fn screen_one_chain() {
        assert_eq!(candidate_names(1), vec!["wallpaper1.mp4", "wallpaper.mp4"]);
    }

    #[test]
    fn screen_two_chain() {
        assert_eq!(
            candidate_names(2),
            vec!["wallpaper2.mp4", "wallpaper1.mp4", "wallpaper.mp4"]
        );
    }

    #[test]
    fn screen_three_chain() {
        assert_eq!(
            candidate_names(3),
            vec!["wallpaper3.mp4", "wallpaper1.mp4", "wallpaper.mp4"]
        );
    }

    #[test]
    fn chooses_first_existing_in_priority_order() {
        let chosen = choose_video(2, |name| name == "wallpaper1.mp4");
        assert_eq!(chosen, "wallpaper1.mp4");
        let chosen = choose_video(2, |name| {
            name == "wallpaper2.mp4" || name == "wallpaper.mp4"
        });
        assert_eq!(chosen, "wallpaper2.mp4");
        let chosen = choose_video(2, |_| false);
        assert_eq!(chosen, "wallpaper2.mp4");
    }
}
