//! The eval's concurrent attempts, and the cost lookups after them (docs/permissions.md).

use super::*;

/// What the attempts run against.
pub(super) struct Eval {
    pub(super) cases: Vec<Labeled>,
    pub(super) toolbox: Toolbox,
    pub(super) endpoint: efference::api::Endpoint,
    pub(super) model: String,
    pub(super) persona: Option<String>,
    pub(super) kind: efference::api::ModelKind,
    pub(super) thinking: efference::api::ThinkingSupport,
    pub(super) runs: usize,
    /// Bounds the attempts in flight across cases.
    pub(super) semaphore: tokio::sync::Semaphore,
}

impl Eval {
    /// Every case's runs, one task per case.
    pub(super) async fn run(self: &Rc<Self>, local: &tokio::task::LocalSet) -> Vec<Attempt> {
        let runs = self.runs;
        let total_attempts = self.cases.len() * runs;
        local
            .run_until(async {
                let mut handles = Vec::with_capacity(self.cases.len());
                // One task per case; its runs go sequentially inside, so run 1 pays
                // the case's cache write and the rest read the warm prefix —
                // concurrent same-case attempts would each pay the write, roughly
                // tripling the cost. Cross-case concurrency is the semaphore's job.
                for case_index in 0..self.cases.len() {
                    let eval = Rc::clone(self);
                    handles.push(tokio::task::spawn_local(async move {
                        eval.run_case(case_index).await
                    }));
                }
                let mut attempts = Vec::with_capacity(total_attempts);
                for handle in handles {
                    attempts.extend(handle.await.expect("eval task never panics"));
                    let done = attempts.len();
                    if done / 50 != done.saturating_sub(runs) / 50 {
                        eprintln!("… {done}/{total_attempts} attempts");
                    }
                }
                attempts
            })
            .await
    }

    /// One case's runs, in order.
    async fn run_case(&self, case_index: usize) -> Vec<Attempt> {
        let mut case_attempts = Vec::with_capacity(self.runs);
        for run in 1..=self.runs {
            let _permit = self
                .semaphore
                .acquire()
                .await
                .expect("semaphore never closes");
            case_attempts.push(self.attempt(case_index, run).await);
        }
        case_attempts
    }

    /// One run of one case: the spotter's answer scored against the expected verdict.
    async fn attempt(&self, case_index: usize, run: usize) -> Attempt {
        let (case, expected, bucket) = &self.cases[case_index];
        let role = role(case);
        let call = efference::log::ToolCall::new(
            case.call.name.clone(),
            "toolu_spotter_eval",
            case.call.input.clone(),
        );
        // Which rung the model is (docs/permissions.md).
        // The persona/model mismatch was rejected before the
        // first request, so this cannot fail here.
        let spot = MountedSpotter::mount_with_persona(
            self.endpoint.clone(),
            &self.model,
            self.kind,
            self.persona.clone(),
        )
        .expect("the rung was validated before the runs began")
        .with_thinking(self.thinking)
        .with_scratch(case.scratch.then(|| PathBuf::from(SCRATCH_PLACEHOLDER)));
        // A case without a recorded cwd keeps the pre-existing
        // behavior (the process cwd, via the rung's fallback);
        // the count is warned about up front.
        let spot = match &case.cwd {
            Some(cwd) => spot.with_directories(efference::policy::Directories::at(Path::new(cwd))),
            None => spot,
        };
        let brief = efference::agent::brief(&self.toolbox, &call.name);
        let start = Instant::now();
        let spotting = spot
            .assess(&role, &call, brief.as_ref(), &[], None, &[])
            .await;
        let ms = start.elapsed().as_millis() as u64;
        let (stance, verdict, ok) = match spotting.origin {
            Origin::Model => {
                let verdict = binary(spotting.read.stance);
                (
                    stance_name(spotting.read.stance).to_string(),
                    verdict.to_string(),
                    Some(verdict == expected),
                )
            }
            _ => ("failed".into(), "failed".into(), None),
        };
        let usage = spotting.usage;
        Attempt {
            id: case.id.clone(),
            run,
            expected: expected.clone(),
            bucket: bucket.clone(),
            stance,
            verdict,
            ok,
            reason: spotting.read.reason,
            ms,
            input_tokens: usage.as_ref().map_or(0, |u| u.usage.input_tokens),
            output_tokens: usage.as_ref().map_or(0, |u| u.usage.output_tokens),
            cache_read: usage
                .as_ref()
                .map_or(0, |u| u.usage.cache_read_input_tokens),
            cache_write: usage
                .as_ref()
                .map_or(0, |u| u.usage.cache_creation_input_tokens),
            generation_id: usage.and_then(|u| u.id),
            cost: None,
            axes: spotting.axes,
            recorded: case.recorded.clone(),
        }
    }

    /// Provider-billed cost, keyed by generation id (docs/transport.md). Records
    /// can lag a beat behind the response, so one delayed retry per id. Runs
    /// concurrently under the same `--concurrency` the attempts pass used;
    /// `Endpoint::fetch_generation_cost` shares a pooled client, so the
    /// semaphore bounds in-flight lookups, not sockets.
    pub(super) async fn price(
        &self,
        local: &tokio::task::LocalSet,
        attempts: &mut [Attempt],
        concurrency: usize,
    ) {
        let mut unpriced = attempts
            .iter()
            .filter(|a| a.generation_id.is_none())
            .count();
        let lookups: Vec<(usize, String)> = attempts
            .iter()
            .enumerate()
            .filter_map(|(i, a)| a.generation_id.clone().map(|id| (i, id)))
            .collect();
        let total = lookups.len();
        eprintln!("cost: fetching {total} generation id(s) (--concurrency {concurrency})");
        let semaphore = Rc::new(tokio::sync::Semaphore::new(concurrency.max(1)));
        let results: Vec<(usize, Result<f64, efference::api::Error>)> = local
            .run_until(async {
                let mut handles = Vec::with_capacity(total);
                for (index, id) in lookups {
                    let semaphore = semaphore.clone();
                    let endpoint = self.endpoint.clone();
                    handles.push(tokio::task::spawn_local(async move {
                        let _permit = semaphore.acquire().await.expect("semaphore never closes");
                        let mut cost = endpoint.fetch_generation_cost(&id).await;
                        if cost.is_err() {
                            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                            cost = endpoint.fetch_generation_cost(&id).await;
                        }
                        (index, cost)
                    }));
                }
                let mut results = Vec::with_capacity(total);
                for handle in handles {
                    results.push(handle.await.expect("cost task never panics"));
                    let done = results.len();
                    if done / 50 != done.saturating_sub(1) / 50 || done == total {
                        eprintln!("… cost {done}/{total} lookups");
                    }
                }
                results
            })
            .await;
        let mut priced = 0usize;
        for (index, cost) in results {
            match cost {
                Ok(usd) => {
                    attempts[index].cost = Some(usd);
                    priced += 1;
                }
                Err(_) => unpriced += 1,
            }
        }
        if unpriced > 0 {
            eprintln!("cost: {priced} attempt(s) priced, {unpriced} unpriced");
        }
    }
}
