//! `horadric google`: sign in to Google for the backup to Google Drive,
//! sign out, say who is signed in, and try the Drive vault end to end.
//! The Settings section (docs/plan/backup.md) does the same with buttons.

use horadric_ui::backup::Vault;
use horadric_ui::google;
use horadric_ui::google_drive::DriveVault;
use horadric_ui::portal_pair::pc_name;

pub fn run(args: &[String]) -> Result<(), String> {
    match args.first().map(String::as_str) {
        Some("sign-in") => {
            println!("Sign in to Google in the browser that just opened.");
            let email = google::sign_in()?;
            println!("Signed in as {email}.");
        }
        Some("sign-out") => {
            google::sign_out();
            println!("Signed out of Google.");
        }
        None | Some("status") => match google::account() {
            Some(email) => println!("Signed in to Google as {email}."),
            None => println!("Not signed in to Google. `horadric google sign-in` signs in."),
        },
        Some("machines") => {
            for m in DriveVault::machines()? {
                println!("{m}");
            }
        }
        Some("try") => try_drive()?,
        Some(other) => {
            return Err(format!(
                "unknown `horadric google {other}`: sign-in, sign-out, status, machines or try"
            ))
        }
    }
    Ok(())
}

/// Puts, lists, gets, replaces and deletes a file in this machine's
/// backup folder, so the Drive client is seen to work before a backup
/// trusts it.
fn try_drive() -> Result<(), String> {
    let vault = DriveVault::open(&pc_name())?;
    let name = "try/horadric.txt";
    let big: Vec<u8> = (0..6 * 1024 * 1024).map(|i| (i % 251) as u8).collect();
    vault.put(name, b"first")?;
    vault.put(name, &big)?;
    if !vault.list()?.iter().any(|n| n == name) {
        return Err(format!("{name} was put but is not listed"));
    }
    if vault.get(name)? != big {
        return Err(format!("{name} came back different"));
    }
    vault.delete(name)?;
    if vault.list()?.iter().any(|n| n == name) {
        return Err(format!("{name} was deleted but is still listed"));
    }
    println!("Google Drive works: put, replace (6 MB), list, get and delete.");
    Ok(())
}
