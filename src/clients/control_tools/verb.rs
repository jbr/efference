//! What a session-control verb is to the sets that list it (docs/tools.md): the
//! [`ControlVerb`] trait each verb implements, the [`VerbCall`] its methods read, and the
//! `verbs!` macro that turns the verb table in `control_tools.rs` into each verb's
//! mcplease `Tool` impl and the three sets.

use std::future::Future;

use anyhow::Result;
use mcplease::traits::ToolOutput;
use mcplease::types::RequestContext;

use super::{ClientExecutor, Control, Executor, HostExecutor};

/// A session-control verb: what it does through each executor (docs/tools.md). Its
/// `Tool` impl is written by `verbs!`, from the verb table, and hands each call to
/// [`client`](Self::client) or [`host`](Self::host) by the executor the mount holds, so
/// no verb matches on [`Executor`] itself.
pub trait ControlVerb: Sized + Send {
    /// What the verb answers.
    type Output: ToolOutput;

    /// The call over the control channel: the command line and `efference mcp`, operator
    /// clients that drive every session the host holds.
    fn client(
        self,
        state: &ClientExecutor,
        call: &VerbCall<'_>,
    ) -> impl Future<Output = Result<Self::Output>> + Send;

    /// The call in-process: a session's own toolbox, which reaches the host it lives in
    /// with the caller's identity. A verb the in-session set does not list is the
    /// operator's, and says so.
    fn host(
        self,
        _state: &HostExecutor,
        call: &VerbCall<'_>,
    ) -> impl Future<Output = Result<Self::Output>> + Send {
        let refused = super::operator_verb(call.verb);
        async move { Err(refused) }
    }
}

/// One call of a verb, as its [`ControlVerb`] methods read it.
pub struct VerbCall<'a> {
    /// The name the verb is advertised under, from the verb table.
    pub verb: &'static str,
    /// The request the call came in: over MCP it carries the caller's `clientInfo`, which
    /// names it as a sender and as a reader; the command line's is empty, and means the
    /// operator.
    pub context: &'a RequestContext,
    /// Whether the call came from the command line rather than from an MCP client: `image`
    /// writes a file there rather than answering with the bytes, and `create` watches
    /// nothing, since the command line holds no mount to watch from (docs/operating.md).
    pub cli: bool,
}

/// Run `verb`, advertised as `name`, through the executor `control` holds: the body of
/// every verb's `Tool::execute`.
pub(super) async fn run<V: ControlVerb>(
    verb: V,
    name: &'static str,
    control: Control,
    context: RequestContext,
) -> Result<V::Output> {
    let call = VerbCall {
        verb: name,
        context: &context,
        cli: control.cli,
    };
    match &control.executor {
        Executor::Client(state) => verb.client(state, &call).await,
        Executor::Host(state) => verb.host(state, &call).await,
    }
}

/// The verb table's expansion (docs/tools.md). A row is `Type "name" [session mcp cli];`:
/// a verb's tool struct, the name it is advertised under, and the sets that list it,
/// `-` in the column of a set that does not. `Type as Variant` names the struct in the
/// sets as `Variant`, the name the command line gives its subcommand. From the rows this
/// writes
///
/// - each struct's mcplease `Tool` impl, which runs it through [`run`]; and
/// - the three sets, each a `tools!` enum of the rows its column marks, in table order:
///   `in_session`, `mcp` and `cli`.
///
/// The rows are taken one at a time, each column adding the row to its set or passing
/// it by, and the sets are written once the rows run out.
macro_rules! verbs {
    (@row $sets:tt $ty:ident as $variant:ident $name:literal [$($column:tt)*]; $($rest:tt)*) => {
        impl ::mcplease::traits::Tool<$crate::clients::control_tools::Control> for $ty {
            type Output = <$ty as $crate::clients::control_tools::verb::ControlVerb>::Output;

            async fn execute(
                self,
                control: $crate::clients::control_tools::Control,
                context: ::mcplease::types::RequestContext,
            ) -> ::anyhow::Result<Self::Output> {
                $crate::clients::control_tools::verb::run(self, $name, control, context).await
            }
        }
        $crate::clients::control_tools::verb::verbs!(
            @session $sets ($ty as $variant, $name) [$($column)*] $($rest)*
        );
    };
    (@row $sets:tt $ty:ident $name:literal [$($column:tt)*]; $($rest:tt)*) => {
        $crate::clients::control_tools::verb::verbs!(
            @row $sets $ty as $ty $name [$($column)*]; $($rest)*
        );
    };
    (@session [[$($s:tt)*] $m:tt $c:tt] $row:tt [session $($column:tt)*] $($rest:tt)*) => {
        $crate::clients::control_tools::verb::verbs!(
            @mcp [[$($s)* $row] $m $c] $row [$($column)*] $($rest)*
        );
    };
    (@session $sets:tt $row:tt [- $($column:tt)*] $($rest:tt)*) => {
        $crate::clients::control_tools::verb::verbs!(@mcp $sets $row [$($column)*] $($rest)*);
    };
    (@mcp [$s:tt [$($m:tt)*] $c:tt] $row:tt [mcp $($column:tt)*] $($rest:tt)*) => {
        $crate::clients::control_tools::verb::verbs!(
            @cli [$s [$($m)* $row] $c] $row [$($column)*] $($rest)*
        );
    };
    (@mcp $sets:tt $row:tt [- $($column:tt)*] $($rest:tt)*) => {
        $crate::clients::control_tools::verb::verbs!(@cli $sets $row [$($column)*] $($rest)*);
    };
    (@cli [$s:tt $m:tt [$($c:tt)*]] $row:tt [cli] $($rest:tt)*) => {
        $crate::clients::control_tools::verb::verbs!(@row [$s $m [$($c)* $row]] $($rest)*);
    };
    (@cli $sets:tt $row:tt [-] $($rest:tt)*) => {
        $crate::clients::control_tools::verb::verbs!(@row $sets $($rest)*);
    };
    (@row [
        [$(($s_ty:ident as $s_variant:ident, $s_name:literal))*]
        [$(($m_ty:ident as $m_variant:ident, $m_name:literal))*]
        [$(($c_ty:ident as $c_variant:ident, $c_name:literal))*]
    ]) => {
        /// The in-session set: the verbs a hosted session's toolbox mounts under
        /// `agents__<verb>` (docs/tools.md), `Provider::Sessions`.
        pub mod in_session {
            use super::Control;
            $(use super::$s_ty as $s_variant;)*

            ::mcplease::tools!(
                cli = false,
                modules = false,
                Control,
                $(($s_variant, $s_name)),*
            );
        }

        /// The `efference mcp` set (docs/operating.md): the agent verbs, served over
        /// stdin/stdout to a harness that mounts the server, with `wait` in place of
        /// `settle`, and the `status` whose struct adds `follow`.
        pub mod mcp {
            use super::Control;
            $(use super::$m_ty as $m_variant;)*

            ::mcplease::tools!(
                cli = false,
                modules = false,
                Control,
                $(($m_variant, $m_name)),*
            );
        }

        /// The command-line set: `efference control <verb>` (docs/operating.md), a clap
        /// subcommand per verb — the agent verbs and `settle`, then the operator's route
        /// verbs.
        pub mod cli {
            use super::Control;
            $(use super::$c_ty as $c_variant;)*

            ::mcplease::tools!(
                modules = false,
                Control,
                $(($c_variant, $c_name)),*
            );
        }
    };
    ($($ty:ident $(as $variant:ident)? $name:literal [$($column:tt)*];)*) => {
        $crate::clients::control_tools::verb::verbs!(
            @row [[] [] []] $($ty $(as $variant)? $name [$($column)*];)*
        );
    };
}

pub(super) use verbs;
