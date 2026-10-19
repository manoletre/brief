//! Explainer videos for the brief. The brief writer gives some items a storyboard (what to
//! learn, beat by beat, from the source it read); for each, the agent writes a ManimGX scene
//! (manimgx.academa.ai: Manim CE's API on a Rust GPU core, with Typst for text and math) and brief
//! renders it as clips, one per beat (briefs/media/DATE/vN/ITEM-K.mp4), which the page plays one
//! at a time as the reader steps through, with the beat's caption under it. `manimgx inspect`
//! checks each beat's layout first (text cut off, overlapping, too small); errors and problems go
//! back to the agent, twice at most.

use crate::agent::{self, Tools};
use crate::store::{Brief, Store};
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

pub const INSTALL: &str = "uv tool install --python 3.14 manimgx";
const ATTEMPTS: usize = 3;
const AT_ONCE: usize = 3;

pub fn manimgx() -> Option<PathBuf> {
    agent::find_bin("manimgx")
}

/// Render every storyboard in the brief that has no video yet. Failures are logged and skipped.
pub async fn render_all(store: &Store, brief: &mut Brief) {
    if std::env::var_os("BRIEF_NO_VIDEO").is_some() {
        return;
    }
    let Some(bin) = manimgx() else {
        if brief.sections.iter().flat_map(|s| &s.items).any(|i| !i.video.is_null()) {
            eprintln!("brief: skipping videos, manimgx isn't installed ({INSTALL})");
        }
        return;
    };
    let Some(date) = brief.date else { return };
    let version = format!("v{}", brief.version.max(1));
    let dir = store.media_dir(date).join(&version);
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let gate = Arc::new(tokio::sync::Semaphore::new(AT_ONCE));
    let mut jobs = Vec::new();
    for (si, s) in brief.sections.iter().enumerate() {
        for (ii, item) in s.items.iter().enumerate() {
            if item.video.is_null() || !item.video_file.is_empty() {
                continue;
            }
            let (store, bin, dir, gate) = (store.clone(), bin.clone(), dir.clone(), gate.clone());
            let (id, board) = (item.id.clone(), item.video.clone());
            jobs.push(tokio::spawn(async move {
                let _permit = gate.acquire().await;
                let r = make(&store, &bin, &dir, &id, &board).await;
                (si, ii, id, r)
            }));
        }
    }
    for j in jobs {
        let Ok((si, ii, id, r)) = j.await else { continue };
        match r {
            Ok(files) => {
                let item = &mut brief.sections[si].items[ii];
                item.video_clips = files.iter().map(|f| format!("{version}/{f}")).collect();
                item.video_file = item.video_clips[0].clone();
            }
            Err(e) => eprintln!("brief: video for {id} failed: {e:#}"),
        }
    }
}

async fn make(store: &Store, bin: &Path, dir: &Path, id: &str, board: &serde_json::Value) -> Result<Vec<String>> {
    let beats = board["beats"].as_array().map_or(0, Vec::len).clamp(1, 6);
    let (a, model) = agent::configured(store);
    let work = store.root.join("work").join(format!("video-{id}-{}", std::process::id()));
    std::fs::create_dir_all(&work)?;
    let mut prompt = scene_prompt(board);
    let mut last_err = String::new();
    for attempt in 0..ATTEMPTS {
        let reply = a.ask(store, &model, &prompt, Tools::None, Duration::from_secs(8 * 60)).await?;
        let code = python_block(&reply.text);
        if let Some(n) = (1..=beats).find(|n| !code.contains(&format!("class Beat{n}("))) {
            last_err = format!("the reply had no `class Beat{n}(m.Scene)`: write one scene per beat, Beat1 to Beat{beats}");
        } else {
            std::fs::write(work.join("scene.py"), &code)?;
            // Layout problems are worth another try, but the last try renders anyway.
            let checked = match inspect(bin, &work, beats).await {
                Ok(()) => Ok(()),
                Err(Problem::Layout(_)) if attempt + 1 == ATTEMPTS => Ok(()),
                Err(Problem::Layout(e) | Problem::Error(e)) => Err(e),
            };
            match checked {
                Ok(()) => match render(bin, &work, dir, id, beats).await {
                    Ok(files) => {
                        let _ = std::fs::remove_dir_all(&work);
                        return Ok(files);
                    }
                    Err(e) => last_err = format!("{e:#}"),
                },
                Err(e) => last_err = e,
            }
        }
        if attempt + 1 < ATTEMPTS {
            prompt = format!(
                "{}\n\n# Your previous attempt failed\n\n```python\n{}\n```\n\n`manimgx inspect` / `manimgx render` said:\n```\n{}\n```\n\nFix it (follow every rule above) and reply with the full corrected code.",
                scene_prompt(board),
                code,
                last_err
            );
        }
    }
    let _ = std::fs::remove_dir_all(&work);
    bail!("{last_err}")
}

enum Problem {
    /// Text cut off by the frame, overlapping, or too small.
    Layout(String),
    /// The scene doesn't run.
    Error(String),
}

async fn run(bin: &Path, work: &Path, args: &[&str], minutes: u64) -> Result<(bool, String)> {
    let mut cmd = tokio::process::Command::new(bin);
    cmd.current_dir(work).args(args).env("PATH", agent::tool_path()).stdin(std::process::Stdio::null()).kill_on_drop(true);
    let out = tokio::time::timeout(Duration::from_secs(minutes * 60), cmd.output()).await.with_context(|| format!("manimgx took over {minutes} minutes"))??;
    let log = String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    let lines: Vec<&str> = log.lines().collect();
    Ok((out.status.success(), lines[lines.len().saturating_sub(40)..].join("\n")))
}

/// Inspect every beat; report the first beat with errors, else all layout problems.
async fn inspect(bin: &Path, work: &Path, beats: usize) -> std::result::Result<(), Problem> {
    let mut layout = String::new();
    for n in 1..=beats {
        let scene = format!("Beat{n}");
        match run(bin, work, &["inspect", "scene.py", &scene], 3).await {
            Ok((true, _)) => {}
            Ok((false, log)) if log.contains("layout:") && !log.contains("Traceback") && !log.contains("Error") => layout.push_str(&format!("{scene}:\n{log}\n")),
            Ok((false, log)) => return Err(Problem::Error(format!("{scene}:\n{log}"))),
            Err(e) => return Err(Problem::Error(format!("{scene}: {e:#}"))),
        }
    }
    if layout.is_empty() { Ok(()) } else { Err(Problem::Layout(layout)) }
}

async fn render(bin: &Path, work: &Path, out_dir: &Path, id: &str, beats: usize) -> Result<Vec<String>> {
    let mut files = Vec::new();
    for n in 1..=beats {
        match render_one(bin, work, out_dir, &format!("{id}-{n}"), &format!("Beat{n}")).await {
            Ok(f) => files.push(f),
            Err(e) => {
                for f in &files {
                    let _ = std::fs::remove_file(out_dir.join(f));
                }
                return Err(e.context(format!("Beat{n}")));
            }
        }
    }
    Ok(files)
}

async fn render_one(bin: &Path, work: &Path, out_dir: &Path, id: &str, scene: &str) -> Result<String> {
    let file = format!("{id}.mp4");
    let out = out_dir.join(&file);
    let out_s = out.display().to_string();
    let (ok, log) = run(bin, work, &["render", "scene.py", scene, "-o", &out_s, "-r", "1280x720", "--fps", "30"], 4).await?;
    // render writes storyboards beside the video; the page doesn't use them.
    for e in std::fs::read_dir(out_dir).into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if name.starts_with(&format!("{id}.storyboard")) {
            let _ = std::fs::remove_file(e.path());
        }
    }
    if !ok || !out.is_file() {
        let _ = std::fs::remove_file(&out);
        bail!("{log}");
    }
    Ok(file)
}

fn python_block(text: &str) -> String {
    let t = text.trim();
    let body = match t.find("```python").map(|i| i + 9).or_else(|| t.find("```").map(|i| i + 3)) {
        Some(start) => {
            let rest = &t[start..];
            &rest[..rest.find("```").unwrap_or(rest.len())]
        }
        None => t,
    };
    body.trim().to_owned()
}

fn scene_prompt(board: &serde_json::Value) -> String {
    let beats = board["beats"].as_array().map_or(1, Vec::len).clamp(1, 6);
    format!(
        r##"Write ManimGX scenes for a short explainer in someone's daily brief. ManimGX (manimgx.academa.ai) has
the API of Manim Community Edition: `import manimgx as m`, then m.Circle, m.MathTex, m.Create, m.UP…

The explainer is a few clips, one per beat. The reader watches one, reads its caption (shown as text under the
video, not in it), then presses Next for the next clip. Each clip gives them the idea of its beat at a glance. It is
a simple diagram that appears and makes one point, not a show: few objects, little motion, nothing decorative.

# The storyboard

{board}

`insight` is what they must leave with. Each beat has `say` (its caption, shown under the video) and `show` (what
the clip draws). Use the real labels and numbers from the storyboard; never invent data.

# Rules (each scene is checked with `manimgx inspect` and rendered as is)

- One scene per beat, in order: `class Beat1(m.Scene):` … `class Beat{beats}(m.Scene):`, each with
  `def construct(self) -> None:`. Start the file with `import manimgx as m` (plus `math`/`random` if needed). No
  files, images, SVGs, sounds or network. Shared drawing code goes in plain functions at the top.
- Each clip starts on a black frame or, to continue the previous beat, by `self.add(...)`-ing the previous beat's
  final drawing at once (no animation), then animates only what this beat adds or changes. Every clip stands on its
  own: nothing left over from another beat.
- No captions, titles or paragraphs in the video: only the diagram and its short labels (1-4 words, numbers).
- Keep it simple: at most ~8 objects on screen, 2-4 animations per clip, each run_time 0.8-1.2 s, then
  `self.wait(1.5)` so it ends on a still frame worth looking at. A clip is 3-7 s.
- Names: m.Text, m.MathTex (math works, typeset with Typst), m.VGroup, m.Rectangle, m.RoundedRectangle(
  corner_radius=0.1), m.Square, m.Circle, m.Dot, m.Line, m.DashedLine, m.Arrow(start, end, buff=0.1), m.Polygon,
  m.SurroundingRectangle, m.Brace(mob, m.DOWN), m.Axes, m.NumberLine, m.DecimalNumber(..., mob_class=m.Text).
  Animations: m.Create, m.Write, m.FadeIn(mob, shift=0.2*m.UP), m.FadeOut, m.Transform, m.GrowFromCenter,
  m.GrowArrow, m.Indicate, m.Circumscribe, m.LaggedStart, and `mob.animate.…`. Not ShowCreation (that's m.Create).
- Look: right after the import
    m.config.background_color = "#000000"
    FG = "#FFFFFF"; ACCENT = "#FF8700"; DIM = "#9A9A9A"; BAD = "#FF5F5F"; GOOD = "#5FD787"; PANEL = "#1C1C1C"
    FONT = "monospace"
  Text in FG with font=FONT, font_size 30-40 (never under 26). ACCENT only for the one thing to look at in that
  beat. Boxes: stroke FG or ACCENT, stroke_width 3, fill PANEL (fill_opacity=1) or none.
- Layout: the frame is 14.2 × 8; keep everything inside x in [-6.5, 6.5], y in [-3.5, 3.5], centred. Never let
  shapes or texts overlap: place with .arrange(m.RIGHT, buff=0.6), .next_to(x, m.DOWN, buff=0.5), and leave room
  for what later beats add. Labels of narrow neighbouring pieces go alternately above and below.

Reply with only the code, in one ```python block."##,
        board = serde_json::to_string_pretty(board).unwrap_or_default()
    )
}
