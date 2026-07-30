use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Result};

use crate::paths;
use crate::store::Store;

pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn open_store() -> Result<Store> {
    let dir = paths::data_dir()?;
    std::fs::create_dir_all(&dir)?;
    Store::open(&paths::db_path()?)
}

fn short(id: &str) -> &str {
    &id[..id.len().min(8)]
}

/// Unix seconds as UTC `YYYY-MM-DD HH:MM`, using the civil-from-days formula so
/// no date crate is needed.
fn fmt_time(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);
    let (hh, mm) = (tod / 3600, (tod % 3600) / 60);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02} {hh:02}:{mm:02}")
}

pub fn ls() -> Result<()> {
    let store = open_store()?;
    let computers = store.list_computers()?;
    if computers.is_empty() {
        println!("no computers yet");
        return Ok(());
    }
    println!("{:<20} {:<10} {:<18} HEAD", "NAME", "STATE", "CREATED");
    for c in computers {
        let head = c.head_id.as_deref().map(short).unwrap_or("-");
        println!(
            "{:<20} {:<10} {:<18} {}",
            c.name,
            "stopped",
            fmt_time(c.created_at),
            head
        );
    }
    Ok(())
}

pub fn history(name: &str) -> Result<()> {
    let store = open_store()?;
    let checkpoints = store.history(name)?;
    if checkpoints.is_empty() {
        println!("{name} has no checkpoints yet");
        return Ok(());
    }
    for cp in checkpoints {
        let label = cp.label.as_deref().unwrap_or("");
        println!("{}  {}  {}", short(&cp.id), fmt_time(cp.created_at), label);
    }
    Ok(())
}

pub fn rm(name: &str, keep_checkpoints: bool) -> Result<()> {
    let store = open_store()?;
    if !store.delete_computer(name)? {
        bail!("no such computer: {name}");
    }
    if keep_checkpoints {
        println!("removed computer {name} (checkpoints kept as orphans)");
    } else {
        let reclaimed = prune(&store)?;
        println!("removed computer {name}, reclaimed {reclaimed} checkpoint(s)");
    }
    Ok(())
}

/// Delete every checkpoint unreachable from any computer, and its image.
/// Shared nodes survive because reachability spans all computers.
fn prune(store: &Store) -> Result<usize> {
    let reachable = store.reachable_checkpoints()?;
    let dead: Vec<_> = store
        .all_checkpoints()?
        .into_iter()
        .filter(|c| !reachable.contains(&c.id))
        .collect();
    for cp in &dead {
        let _ = std::fs::remove_file(&cp.image_path);
    }
    let ids: Vec<String> = dead.iter().map(|c| c.id.clone()).collect();
    store.delete_checkpoints(&ids)?;
    Ok(dead.len())
}

pub fn gc() -> Result<()> {
    let store = open_store()?;
    match prune(&store)? {
        0 => println!("nothing to prune"),
        n => println!("pruned {n} checkpoint(s)"),
    }
    Ok(())
}

pub fn clone(source: &str, name: &str) -> Result<()> {
    let store = open_store()?;
    if store.get_computer(name)?.is_some() {
        bail!("computer '{name}' already exists");
    }

    if Path::new(source).is_file() {
        // Adopt an external image as a new root checkpoint.
        let id = Store::new_checkpoint_id();
        let dst = paths::checkpoint_image(&id)?;
        std::fs::create_dir_all(dst.parent().unwrap())?;
        std::fs::copy(source, &dst)?;
        let image = dst.to_string_lossy().into_owned();
        store.add_checkpoint(&id, None, None, &image, now())?;
        store.create_computer(name, Some(&id), now())?;
        println!(
            "cloned image {source} -> computer {name} (root {})",
            short(&id)
        );
    } else {
        // Branch from an existing checkpoint: a new ref sharing the node.
        let cp = store.resolve_ref(source)?;
        store.create_computer(name, Some(&cp.id), now())?;
        println!("cloned {source} -> computer {name} (at {})", short(&cp.id));
    }
    Ok(())
}
