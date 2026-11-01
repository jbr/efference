//! Dispatch for `efference dev`: each [`DevCommand`] to the instrument that serves it.

use super::*;

/// Run one `dev` verb.
// One `match` over the verb, read by variant: each arm hands its verb to the instrument that
// serves it, and the few that carry a body of their own resolve a target and call one.
#[allow(clippy::too_many_lines)]
pub(crate) async fn run(command: DevCommand) -> CliResult {
    match command {
        DevCommand::SessionForm { embedded } => {
            if embedded {
                efference::tui::run_session_form_prototype().await?;
            } else {
                efference::tui::session_form::run()?;
            }
            Ok(())
        }
        DevCommand::Open { print } => open(print).await,
        DevCommand::Cache { session, read, all } => cache_report::cache(session, read.name, all),
        DevCommand::Stops { session, read, all } => {
            stop_report::stops_report(session, read.name, all)
        }
        DevCommand::Digest {
            session,
            read,
            upto,
            seqs,
        } => {
            let target = session
                .or(read.name)
                .ok_or_else(|| CliError::from("name a session to digest"))?;
            let (path, _) = resolve_session_log(&target)?;
            let entries = efference::record::persist::load(&path)?;
            let options = efference::digest::Options {
                upto,
                seqs,
                ..Default::default()
            };
            print!("{}", efference::digest::render(&entries, options));
            Ok(())
        }
        DevCommand::Compact {
            session,
            read,
            upto,
            cut,
        } => {
            let target = session
                .or(read.name)
                .ok_or_else(|| CliError::from("name a session to compact"))?;
            dev_compact::compact(&target, upto, cut).await
        }
        DevCommand::Models {
            connection,
            urls,
            raw,
        } => dev_models::models(&connection, &urls, raw).await,
        DevCommand::History {
            cwd,
            config,
            connection,
        } => dev_models::history(cwd, config.as_deref(), connection.as_deref()),
        DevCommand::View {
            session,
            read,
            theme,
            images,
        } => {
            view(
                session,
                read.name,
                display::over(display()?, theme, images, false),
            )
            .await
        }
        DevCommand::Gallery {
            theme,
            images,
            stream,
            chunk,
            interval,
            frame_cost,
        } => {
            let display = display::over(display()?, theme, images, frame_cost);
            gallery(display, stream, chunk, interval).await
        }
        DevCommand::Wire {
            path,
            messages,
            format,
            dialect,
            config,
        } => {
            let rendered = wire(path, messages, format, dialect, config).await?;
            println!("{rendered}");
            Ok(())
        }
        DevCommand::Triage(args) => triage_replay::triage(args).await,
        DevCommand::PrimerLadder(args) => primer_ladder::ladder(args).await,
        DevCommand::Spotter(args) => serve::spotter_check(args).await,
        DevCommand::SpotterLog {
            path,
            stances,
            summary,
            list,
            no_fallback,
        } => spot_report::spotter_log(path, &stances, summary, list, no_fallback),
        DevCommand::SpotterEval(args) => {
            if let Some(out) = &args.dump_states {
                return spotter_eval::dump_states(
                    &args.cases,
                    args.filter.as_deref(),
                    args.limit,
                    out,
                );
            }
            spotter_eval::spotter_eval(args).await
        }
    }
}
