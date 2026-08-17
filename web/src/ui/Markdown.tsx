// Prose rendering (docs/web-client.md). Assistant and user turns are
// markdown and are rendered as such; every other body on the page is not
// prose and does not come through here — thinking is line-structured (a
// reflow turns "- a\n- b" into "- a - b"), and tool output is tool output.
//
// The renderer produces a React element tree, never an HTML string: the
// text is model-authored, and a page holding a session credential must
// never turn untrusted content into markup (backlog/web/untrusted-output.md).
// Two properties of react-markdown carry that, and both are defaults rather
// than configuration — raw HTML is escaped unless `rehype-raw` is added, and
// `defaultUrlTransform` passes only http/https/irc/ircs/mailto/xmpp, so a
// `javascript:` or `data:` link in a model's output does not survive into an
// href. Neither should be switched off without reading that unit.

import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";

export function Markdown({ children }: { children: string }) {
  return (
    <div className="prose">
      <ReactMarkdown remarkPlugins={[remarkGfm]}>{children}</ReactMarkdown>
    </div>
  );
}
