//! Dispatch for `efference dev`: each [`DevCommand`] to the instrument that serves it.

use super::*;

/// Run one `dev` verb.
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
            let entries = efference::persist::load(&path)?;
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
        DevCommand::Triage {
            path,
            seq,
            repeat,
            limit,
            dry_run,
            model,
            triage_model,
            triage_thinking,
            triage_auto_choice,
            triage_effort,
            view,
            out,
            table,
        } => {
            let triage = triage_model.map(|model| efference::triage::TriageModel {
                model,
                thinking: triage_thinking,
                auto_choice: triage_auto_choice,
                effort: triage_effort,
            });
            let view = match view {
                Some(view) => efference::triage::TriageView::parse(&view).ok_or_else(|| {
                    format!("--view {view}: expected transcript, text, tail:N or intent")
                })?,
                None if triage.is_some() => efference::triage::TriageView::Text,
                None => efference::triage::TriageView::Transcript,
            };
            triage_replay::triage(
                path, seq, repeat, limit, dry_run, &model, triage, view, out, table,
            )
            .await
        }
        DevCommand::PrimerLadder {
            path,
            seq,
            gaps,
            dry_run,
            model,
            triage_model,
            triage_thinking,
            triage_auto_choice,
            triage_effort,
            out,
        } => {
            let triage = triage_model.map(|model| efference::triage::TriageModel {
                model,
                thinking: triage_thinking,
                auto_choice: triage_auto_choice,
                effort: triage_effort,
            });
            primer_ladder::ladder(path, seq, &gaps, dry_run, &model, triage, out).await
        }
        DevCommand::Spotter {
            scope,
            watch_for,
            paths,
            model,
            probe,
            tool,
            input,
            config,
            show_move,
            no_brief,
            command,
        } => {
            serve::spotter_check(
                scope,
                watch_for,
                paths,
                model.as_deref(),
                probe,
                tool,
                input,
                config,
                show_move,
                no_brief,
                command.join(" "),
            )
            .await
        }
        DevCommand::SpotterLog {
            path,
            stances,
            summary,
            list,
            no_fallback,
        } => spot_report::spotter_log(path, &stances, summary, list, no_fallback),
        DevCommand::SpotterEval {
            cases,
            labels,
            model,
            runs,
            out,
            limit,
            filter,
            concurrency,
            persona,
            no_cost,
            against_recorded,
            dump_states,
        } => {
            if let Some(out) = dump_states {
                return spotter_eval::dump_states(&cases, filter.as_deref(), limit, &out);
            }
            spotter_eval::spotter_eval(
                cases,
                labels,
                model,
                runs,
                out,
                limit,
                filter,
                concurrency,
                persona,
                no_cost,
                against_recorded,
            )
            .await
        }
    }
}
