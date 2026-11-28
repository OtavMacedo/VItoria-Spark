use std::path::Path;

use anyhow::{Context, Result};

use crate::{Console, Unimplemented};

#[cfg(windows)]
pub fn run(
    exe: &Path,
    unimplemented: Unimplemented,
    strict: bool,
    execute: bool,
    console: Console,
) -> Result<()> {
    // The card maker's settings (perfis/), before anything reads a setting.
    let profile = victory_gpu::vendor::apply();
    // A run asked to alternate a switch that cannot would measure nothing.
    victory_gpu::switches::check_ab().map_err(anyhow::Error::msg)?;
    // The session log (logs/<time>-<pid>/): eventos.jsonl and the summary.
    victory_core::session::start(
        victory_core::session::BuildInfo {
            version: env!("CARGO_PKG_VERSION"),
            git: env!("SPARK_GIT_HASH"),
            profile: if cfg!(feature = "diagnostics") {
                "diag"
            } else if cfg!(debug_assertions) {
                "debug"
            } else {
                "release"
            },
        },
        exe.parent(),
    );
    profile.log();
    let result = run_title(exe, unimplemented, strict, execute, console);
    victory_core::session::finish(&match &result {
        Ok(()) => "o executor terminou".to_owned(),
        Err(e) => format!("erro: {e:#}"),
    });
    result
}

#[cfg(windows)]
fn run_title(
    exe: &Path,
    unimplemented: Unimplemented,
    strict: bool,
    execute: bool,
    console: Console,
) -> Result<()> {
    use std::sync::Arc;
    use victory_hle::{HleResolver, HostPolicy};
    use victory_loader::{BindReport, UnimplementedPolicy, WithTitleDlls};

    // A title that relaunched itself: the old process must be gone first.
    victory_hle::modules::lifecycle::wait_for_previous();
    let cfg = victory_core::Config::load()?;
    let original = std::fs::read(exe).with_context(|| format!("reading {}", exe.display()))?;
    let mut prepared = victory_loader::prepare(&original)?;
    let hash = prepared.original_hash.to_hex();
    let build = victory_game::identify(&hash);
    match build {
        Some(build) => println!("build          {} ({hash})", build.label),
        None => println!("build          unknown ({hash})"),
    }
    let output = apply_title_patches(&mut prepared, build, console)?;

    let images = cfg.cache_subdir("images")?;
    victory_hle::modules::d3d12x::use_shader_cache(cfg.cache_subdir("shaders")?);
    let image = victory_loader::load(&prepared, &images)?;
    // The program is mapped from the copy `load` wrote: the file's bytes
    // and their patched copy are not read again, and were held for the
    // whole run (two blocks of 114.7 MB each for the title, in
    // physical memory: run 65's memory map).
    drop(prepared);
    drop(original);
    victory_hle::runtime::set_main_image(image.base());
    victory_hle::runtime::set_image_path(image.base(), exe);
    println!(
        "mapped at      {:#x} (preferred {:#x})",
        image.base(),
        image.info().image_base
    );
    let resolver = HleResolver::new(if strict {
        HostPolicy::none()
    } else {
        HostPolicy::default()
    });
    let wanted = |dll: &str| title_dll(&resolver.host, dll);
    let title_dir = exe.parent().unwrap_or(Path::new("."));
    // A missing manifest leaves id 0 and no storage quota.
    let manifest =
        std::fs::read_to_string(title_dir.join("MicrosoftGame.config")).unwrap_or_default();
    let title = victory_hle::runtime::Title::from_manifest(&manifest);
    let storage = cfg.data_subdir(&format!("{:08X}/storage", title.id))?;
    println!(
        "title          {:08X}, storage {} ({} MB)",
        title.id,
        storage.display(),
        title.storage_bytes >> 20
    );
    // The console's fixed drives: the package as `G:`, temporary files `T:`.
    victory_hle::modules::files::mount_at('G', title_dir);
    let temp = cfg.data_subdir(&format!("{:08X}/temp", title.id))?;
    victory_hle::modules::files::mount_at('T', &temp);
    if let Some(size) = output {
        ui_follows_output(&storage, size);
    }
    victory_hle::runtime::set_title(victory_hle::runtime::Title { storage, ..title });
    victory_hle::console::set(match console {
        Console::SeriesX => victory_hle::console::Console::SeriesX,
        Console::SeriesS => victory_hle::console::Console::SeriesS,
    });
    println!("console        {}", victory_hle::console::current().name());
    let dlls = victory_loader::load_title_dlls(image.info(), title_dir, &images, &wanted)?;
    for (name, dll) in &dlls {
        println!("title dll      {name} at {:#x}", dll.base());
        victory_hle::runtime::set_image_path(dll.base(), &title_dir.join(name));
    }

    let policy = match unimplemented {
        Unimplemented::Trap => UnimplementedPolicy::Trap,
        Unimplemented::Zero => UnimplementedPolicy::ReturnZero,
    };
    let with_dlls = WithTitleDlls {
        inner: &resolver,
        dlls: &dlls,
    };
    let show = |name: &str, report: BindReport| {
        println!(
            "bound          {name}: {} hle, {} host, {} title, {} missing",
            report.hle,
            report.host,
            report.title,
            report.missing.len()
        );
        for m in &report.missing {
            println!("  missing      {}!{}", m.dll, m.symbol);
        }
    };
    for (name, dll) in &dlls {
        show(name, dll.bind_imports(&with_dlls, policy)?);
    }
    let exe_name = exe.file_name().unwrap_or_default().to_string_lossy();
    show(&exe_name, image.bind_imports(&with_dlls, policy)?);
    if !execute {
        println!("not executing (pass --execute to start the title)");
        return Ok(());
    }
    victory_loader::report_crashes();
    victory_loader::log_crashes_to(temp.with_file_name("crashes.log"));
    victory_hle::modules::xmem::keep_in_memory();
    victory_hle::runtime::use_console_timers();
    #[cfg(feature = "diagnostics")]
    diagnostics(&image, &cfg);
    let late = LateDlls {
        host: resolver.host.clone(),
        title_dir: title_dir.to_owned(),
        cache_dir: images,
        policy,
        mapped: std::sync::Mutex::new(dlls.clone()),
        lock: LoaderLock::default(),
    };
    victory_hle::runtime::set_title_dll_loader(Box::new(move |file| {
        late.load(file, show).map_err(|e| format!("{e:#}"))
    }));
    // SAFETY: imports are bound; from here on we run title code.
    let main = unsafe { Arc::new(image).spawn_main(dlls)? };
    let code = main
        .join()
        .map_err(|_| anyhow::anyhow!("title main thread panicked"))??;
    println!("title exited with {code}");
    Ok(())
}

/// The debugging aids (README): scheduled stack dumps, watched values and
/// memory saved on a crash.
#[cfg(all(windows, feature = "diagnostics"))]
fn diagnostics(image: &victory_loader::LoadedImage, cfg: &victory_core::Config) {
    victory_loader::dump_stacks_on_schedule();
    victory_hle::modules::audio::capture::start();
    watch(image.base(), image.info().size_of_image as usize);
    // Debugging aid: `VICTORY_CRASH_SAVE=<rva>:<len>,...` (hex) writes those
    // title ranges to `crash-<rva>.bin` in the cache dir on a crash.
    let size = image.info().size_of_image as usize;
    let ranges = std::env::var("VICTORY_CRASH_SAVE").unwrap_or_default();
    let hex = |s: &str| usize::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok();
    victory_loader::save_on_crash(
        ranges
            .split(',')
            .filter_map(|r| r.split_once(':'))
            .filter_map(|(rva, len)| Some((hex(rva)?, hex(len)?)))
            .filter(|&(rva, len)| rva.checked_add(len).is_some_and(|end| end <= size))
            .map(|(rva, len)| {
                let file = cfg.cache_dir.join(format!("crash-{rva:x}.bin"));
                (image.base() + rva, len, file)
            })
            .collect(),
    );
}

/// Debugging aid: `VICTORY_WATCH=<rva>,...` (hex) logs the u32 at each
/// title+rva whenever it changes, polled every 5 ms (an engine's state, say).
#[cfg(all(windows, feature = "diagnostics"))]
fn watch(base: usize, size: usize) {
    let Ok(list) = std::env::var("VICTORY_WATCH") else {
        return;
    };
    let rvas: Vec<usize> = list
        .split(',')
        .filter_map(|r| usize::from_str_radix(r.trim().trim_start_matches("0x"), 16).ok())
        .filter(|&rva| rva + 4 <= size)
        .collect();
    std::thread::spawn(move || {
        let mut seen = vec![None; rvas.len()];
        loop {
            for (&rva, seen) in rvas.iter().zip(&mut seen) {
                // SAFETY: four bytes inside the mapped image, checked above.
                let value = unsafe { ((base + rva) as *const u32).read_volatile() };
                if *seen != Some(value) {
                    eprintln!("VIctory: title+{rva:#x} = {value:#x}");
                    *seen = Some(value);
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    });
}

/// DLLs the host or our runtime provide are never taken from the title.
#[cfg(windows)]
fn title_dll(host: &victory_hle::HostPolicy, dll: &str) -> bool {
    !host.allows(dll) && !victory_hle::registry().iter().any(|m| m.serves(dll))
}

/// Title DLLs the title loads itself (`LoadLibrary`) that nothing imports,
/// e.g. The title's upscaler behind its FidelityFX loader.
#[cfg(windows)]
struct LateDlls {
    host: victory_hle::HostPolicy,
    title_dir: std::path::PathBuf,
    cache_dir: std::path::PathBuf,
    policy: victory_loader::UnimplementedPolicy,
    /// Every title DLL mapped so far, to bind new ones against.
    mapped: std::sync::Mutex<Vec<(String, victory_loader::LoadedImage)>>,
    lock: LoaderLock,
}

#[cfg(windows)]
impl LateDlls {
    /// Every `LoadLibrary` of the title comes here first. Returns a title
    /// image called `file`, mapping it and the title DLLs it needs first,
    /// binding them and running their `DllMain` on the calling thread, as
    /// the Windows loader would; `None` if `file` is no title image.
    fn load(
        &self,
        file: &str,
        show: impl Fn(&str, victory_loader::BindReport),
    ) -> Result<Option<usize>> {
        use victory_hle::runtime::{forget_image, image_named, set_image_path};
        // Waits while another thread initialises a DLL; a `DllMain` that
        // loads another DLL re-enters.
        self.lock.hold(|| {
            if let Some(base) = image_named(file) {
                return Ok(Some(base));
            }
            let new = {
                let mut mapped = self.mapped.lock().unwrap_or_else(|e| e.into_inner());
                let wanted = |dll: &str| title_dll(&self.host, dll) && image_named(dll).is_none();
                let new = victory_loader::load_title_dll(
                    file,
                    &self.title_dir,
                    &self.cache_dir,
                    &wanted,
                )?;
                let hle = victory_hle::HleResolver::new(self.host.clone());
                let old = victory_loader::WithTitleDlls {
                    inner: &hle,
                    dlls: &mapped,
                };
                let resolver = victory_loader::WithTitleDlls {
                    inner: &old,
                    dlls: &new,
                };
                for (name, dll) in &new {
                    println!(
                        "title dll      {name} at {:#x} (loaded by the title)",
                        dll.base()
                    );
                    show(name, dll.bind_imports(&resolver, self.policy)?);
                    set_image_path(dll.base(), &self.title_dir.join(name));
                }
                // Visible before their `DllMain`s run, which may load more.
                mapped.extend(new.iter().cloned());
                new
            };
            let failed = new.iter().enumerate().find_map(|(i, (name, dll))| {
                // SAFETY: bound above, dependencies attached first.
                let attached = unsafe { dll.attach() };
                attached
                    .err()
                    .map(|e| (i, e.context(format!("attaching {name}"))))
            });
            let Some((i, error)) = failed else {
                return Ok(new.last().map(|(_, dll)| dll.base()));
            };
            // As `LoadLibrary` does when a `DllMain` fails: detach what this
            // load attached (the failed one too) and unmap all of it.
            let bases: Vec<_> = new.iter().map(|(_, d)| d.base()).collect();
            self.mapped
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .retain(|(_, d)| !bases.contains(&d.base()));
            for (_, done) in new[..=i].iter().rev() {
                // SAFETY: attached (or failing to) on this thread just now.
                unsafe { done.detach() };
            }
            for (_, dll) in new {
                forget_image(dll.base());
                // SAFETY: detached, and no title code holds it yet.
                unsafe { dll.unload() };
            }
            Err(error)
        })
    }
}

/// The Windows loader lock's shape: one thread at a time, re-entered by
/// the thread that holds it.
#[cfg(windows)]
#[derive(Default)]
struct LoaderLock {
    owner: std::sync::Mutex<(Option<std::thread::ThreadId>, usize)>,
    free: std::sync::Condvar,
}

#[cfg(windows)]
impl LoaderLock {
    fn hold<T>(&self, f: impl FnOnce() -> T) -> T {
        let me = std::thread::current().id();
        let lock = || self.owner.lock().unwrap_or_else(|e| e.into_inner());
        {
            let mut owner = lock();
            while owner.0.is_some_and(|o| o != me) {
                owner = self.free.wait(owner).unwrap_or_else(|e| e.into_inner());
            }
            *owner = (Some(me), owner.1 + 1);
        }
        let result = f();
        let mut owner = lock();
        owner.1 -= 1;
        if owner.1 == 0 {
            owner.0 = None;
            self.free.notify_one();
        }
        result
    }
}

#[cfg(all(test, windows))]
mod tests {
    use std::sync::{Arc, Mutex};

    #[test]
    fn loader_lock_is_reentrant_and_exclusive() {
        let lock = Arc::new(super::LoaderLock::default());
        assert_eq!(lock.hold(|| lock.hold(|| 7)), 7);
        let order = Arc::new(Mutex::new(Vec::new()));
        let other = lock.hold(|| {
            let (theirs, their_order) = (lock.clone(), order.clone());
            let other = std::thread::spawn(move || {
                theirs.hold(|| their_order.lock().unwrap().push(2));
            });
            std::thread::sleep(std::time::Duration::from_millis(50));
            order.lock().unwrap().push(1);
            other
        });
        other.join().unwrap();
        assert_eq!(*order.lock().unwrap(), [1, 2]);
    }
}

#[cfg(not(windows))]
pub fn run(_: &Path, _: Unimplemented, _: bool, _: bool, _: Console) -> Result<()> {
    anyhow::bail!("running titles requires Windows")
}

/// Bytes as lower-case hex.
#[cfg(windows)]
fn hex_str(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The known build's code patches (`VICTORY_TITLE_PATCHES=0`: none), then
/// any from `VICTORY_TITLE_PATCH=<rva>:<old hex>:<new hex>,...`, then the
/// output size's ([`output_size_patches`]). The old bytes must be there
/// (another build of the title is refused), and the patched copy has a file
/// name of its own. Returns the output size the image was patched for.
#[cfg(windows)]
fn apply_title_patches(
    prepared: &mut victory_loader::PreparedImage,
    build: Option<&victory_game::KnownBuild>,
    console: Console,
) -> Result<Option<victory_game::output::OutputSize>> {
    let mut patches = Vec::new();
    let mut names = Vec::new();
    if std::env::var("VICTORY_TITLE_PATCHES").as_deref() != Ok("0") {
        for p in build.map_or(&[][..], |b| b.patches) {
            patches.push((p.rva, p.expect.to_vec(), p.write.to_vec()));
            names.push(p.name.to_owned());
        }
    }
    let spec = std::env::var("VICTORY_TITLE_PATCH").unwrap_or_default();
    for one in spec.split(',').filter(|p| !p.trim().is_empty()) {
        let parts: Vec<&str> = one.split(':').collect();
        anyhow::ensure!(
            parts.len() == 3,
            "VICTORY_TITLE_PATCH entry is not <rva>:<old>:<new>: {one}"
        );
        let rva = u32::from_str_radix(parts[0].trim().trim_start_matches("0x"), 16)?;
        patches.push((rva, from_hex(parts[1])?, from_hex(parts[2])?));
        names.push("VICTORY_TITLE_PATCH".to_owned());
    }
    let output = output_size_patches(build, console)?;
    if let Some((_, _, more)) = &output {
        for p in more {
            patches.push((p.rva, p.expect.clone(), p.write.clone()));
            names.push(p.name.to_owned());
        }
    }
    if patches.is_empty() {
        return Ok(None);
    }
    // One call: every expected run is checked before any is written, and
    // the copy's name covers the whole set.
    prepared.patch(&patches)?;
    let size = output.as_ref().map(|(size, _, _)| *size);
    if let Some((size, scene, _)) = output {
        victory_gpu::dxgi::set_output_size(size.width, size.height);
        println!(
            "display        {}x{} SDR, scene {}x{}",
            size.width, size.height, scene.width, scene.height
        );
    }
    for ((rva, old, new), name) in patches.iter().zip(names) {
        println!(
            "title patch    +{rva:#x} {name}: {} -> {}",
            hex_str(old),
            hex_str(new)
        );
    }
    Ok(size)
}

/// Bytes from hex digits.
#[cfg(windows)]
fn from_hex(s: &str) -> Result<Vec<u8>> {
    let s = s.trim();
    anyhow::ensure!(s.len().is_multiple_of(2), "odd hex length: {s}");
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(anyhow::Error::from))
        .collect()
}

/// `VICTORY_OUTPUT_RESOLUTION=1440p|<W>x<H>` (16:9, 2560x1440 to 3840x2160;
/// off by default): a Series X output of that size instead of 3840x2160,
/// made by the title itself, with its scene at `VICTORY_SCENE_SCALE` of it
/// (`0.75` or `3/4`, 0.5 to 1; default 1, the scene at the output's size).
/// A smaller output is refused, as the title stops at startup with one: a
/// 1920x1080 scene is `1440p` with `3/4`. Needs a known build with its
/// output sites and the Series X profile; the title's UI follows the output
/// through its settings file ([`ui_follows_output`]). Returns the output,
/// the scene and the patches.
#[cfg(windows)]
fn output_size_patches(
    build: Option<&victory_game::KnownBuild>,
    console: Console,
) -> Result<
    Option<(
        victory_game::output::OutputSize,
        victory_game::output::OutputSize,
        Vec<victory_game::output::TitlePatch>,
    )>,
> {
    use victory_game::output::{OutputSize, output_patches, parse_scene_scale};
    let Ok(asked) = std::env::var("VICTORY_OUTPUT_RESOLUTION") else {
        return Ok(None);
    };
    if asked.trim().is_empty() || asked.trim() == "0" {
        return Ok(None);
    }
    let size = OutputSize::parse(&asked).map_err(anyhow::Error::msg)?;
    let scale = match std::env::var("VICTORY_SCENE_SCALE") {
        Ok(v) if !v.trim().is_empty() => parse_scene_scale(&v).map_err(anyhow::Error::msg)?,
        _ => 1.0,
    };
    // The patched display mode is the one the title picks on a Series X.
    anyhow::ensure!(
        matches!(console, Console::SeriesX),
        "VICTORY_OUTPUT_RESOLUTION needs the Series X profile"
    );
    let sites = build
        .and_then(|b| b.output.as_ref())
        .context("VICTORY_OUTPUT_RESOLUTION: the output size of this build is not known")?;
    let patches = output_patches(sites, size, scale, scanout_reservation_bytes(size)?);
    Ok(Some((size, size.scene(scale), patches)))
}

/// With the output patched to `size`: the UI blocks of the title's settings
/// file (`<storage>/settings/settings.xml`, [`UI_SETTINGS_BLOCKS`]) get that
/// size, so its UI is laid out for the output and not for 3840x2160. Only
/// those values change; before the first change the file is copied to
/// `settings.xml.original` beside it. A missing file (the title has not
/// made it yet) or one in an unknown form is left alone, and what was done
/// is printed either way; a failure is printed and the run goes on.
///
/// [`UI_SETTINGS_BLOCKS`]: victory_game::output::UI_SETTINGS_BLOCKS
#[cfg(windows)]
fn ui_follows_output(storage: &Path, size: victory_game::output::OutputSize) {
    let file = storage.join("settings").join("settings.xml");
    match ui_settings_follow(&file, size) {
        Ok(line) => println!("ui settings    {line}"),
        Err(error) => println!("ui settings    unchanged, {error:#}"),
    }
}

/// [`ui_follows_output`] for `file`; returns what was done.
#[cfg(any(windows, test))]
fn ui_settings_follow(file: &Path, size: victory_game::output::OutputSize) -> Result<String> {
    use victory_game::output::ui_settings_for;
    const BOM: &[u8] = b"\xef\xbb\xbf";
    let (width, height) = (size.width, size.height);
    let bytes = match std::fs::read(file) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(format!(
                "{} not made yet: the title makes it, and its UI follows the output from \
                 the next start",
                file.display()
            ));
        }
        Err(error) => return Err(error).with_context(|| format!("reading {}", file.display())),
    };
    let (bom, body) = match bytes.strip_prefix(BOM) {
        Some(body) => (BOM, body),
        None => (&[][..], &bytes[..]),
    };
    let text =
        std::str::from_utf8(body).with_context(|| format!("{} is not UTF-8", file.display()))?;
    let ui = ui_settings_for(text, size);
    let missing = if ui.missing.is_empty() {
        String::new()
    } else {
        format!(" ({} not found)", ui.missing.join(", "))
    };
    if ui.changed.is_empty() {
        return Ok(format!(
            "already {width}x{height} in {}{missing}",
            file.display()
        ));
    }
    let backup = file.with_file_name("settings.xml.original");
    if !backup.exists() {
        std::fs::copy(file, &backup).with_context(|| format!("keeping {}", backup.display()))?;
    }
    // Written beside it and renamed over it: never half a file.
    let new = file.with_file_name("settings.xml.new");
    std::fs::write(&new, [bom, ui.text.as_bytes()].concat())
        .with_context(|| format!("writing {}", new.display()))?;
    std::fs::rename(&new, file).with_context(|| format!("replacing {}", file.display()))?;
    let changes: Vec<String> = ui
        .changed
        .iter()
        .map(|(block, w, h)| format!("{block} {w}x{h} -> {width}x{height}"))
        .collect();
    Ok(format!(
        "{} in {} (the first file is kept as {}){missing}",
        changes.join(", "),
        file.display(),
        backup.display()
    ))
}

/// The bytes the title reserves for its two scanout buffers at `size`: two
/// 64 KiB-aligned, tiled RGBA8 surfaces, the whole rounded up to 2 MiB as
/// the title's allocation check expects.
#[cfg(any(windows, test))]
fn scanout_reservation_bytes(size: victory_game::output::OutputSize) -> Result<u32> {
    use victory_formats::texture::{Dimension, Swizzle, Texture, TextureDesc};
    let texture = Texture::new(&TextureDesc {
        dimension: Dimension::Tex2d,
        width: size.width,
        height: size.height,
        depth_or_array: 1,
        mips: 1,
        // DXGI_FORMAT_R8G8B8A8_UNORM.
        format: 28,
        samples: 1,
        swizzle: Swizzle::SW_64KB_D_X,
    })
    .map_err(|error| anyhow::anyhow!("scanout layout: {error:?}"))?;
    Ok(u32::try_from(
        (texture.size.next_multiple_of(64 << 10) * 2).next_multiple_of(2 << 20),
    )?)
}

#[cfg(test)]
mod output_tests {
    use super::{scanout_reservation_bytes, ui_settings_follow};
    use victory_game::output::{LEGACY_CANVAS, OutputSize};

    /// The stock size reproduces the stock reservation (64 MiB); smaller
    /// outputs reserve less, in 2 MiB steps.
    #[test]
    fn scanout_reservations() {
        assert_eq!(scanout_reservation_bytes(LEGACY_CANVAS).unwrap(), 64 << 20);
        for (text, mib) in [("1440p", 30), ("3200x1800", 48)] {
            let size = OutputSize::parse(text).unwrap();
            let bytes = scanout_reservation_bytes(size).unwrap();
            assert_eq!(bytes % (2 << 20), 0, "{text}");
            assert!(u64::from(bytes) >= 2 * u64::from(size.width * size.height * 4));
            assert_eq!(bytes >> 20, mib, "{text}");
        }
    }

    /// The settings file gets the output's size in its two UI blocks, the
    /// first file is kept beside it (once), and a BOM stays.
    #[test]
    fn the_settings_file_follows_the_output() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("settings.xml");
        let backup = dir.path().join("settings.xml.original");
        let size = OutputSize::parse("1440p").unwrap();
        let line = ui_settings_follow(&file, size).unwrap();
        assert!(line.contains("not made yet"), "{line}");
        assert!(!file.exists());

        let stock = "\u{feff}<s><uiResolution><width value=\"0\" /><height value=\"0\" />\
                     </uiResolution><resolutionFullscreen><width value=\"0\" />\
                     <height value=\"0\" /></resolutionFullscreen></s>";
        std::fs::write(&file, stock).unwrap();
        let line = ui_settings_follow(&file, size).unwrap();
        assert!(line.contains("uiResolution 0x0 -> 2560x1440"), "{line}");
        assert!(
            line.contains("resolutionFullscreen 0x0 -> 2560x1440"),
            "{line}"
        );
        let written = std::fs::read_to_string(&file).unwrap();
        let expected = stock
            .replace("<width value=\"0\"", "<width value=\"2560\"")
            .replace("<height value=\"0\"", "<height value=\"1440\"");
        assert_eq!(written, expected);
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), stock);
        assert!(!dir.path().join("settings.xml.new").exists());

        // Another size later: the file changes, the first copy stays.
        let line = ui_settings_follow(&file, LEGACY_CANVAS).unwrap();
        assert!(
            line.contains("uiResolution 2560x1440 -> 3840x2160"),
            "{line}"
        );
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), stock);
        let line = ui_settings_follow(&file, LEGACY_CANVAS).unwrap();
        assert!(line.starts_with("already 3840x2160"), "{line}");

        std::fs::write(&file, b"\xff\xfe<").unwrap();
        assert!(ui_settings_follow(&file, size).is_err());
    }
}
