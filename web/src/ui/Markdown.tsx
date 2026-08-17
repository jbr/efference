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

import type { Components } from "react-markdown";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { Code } from "./Code.tsx";

/** A fenced block goes through the same highlighter as a tool result body,
 * so prose code and tool code are coloured by one language map rather than
 * two. Only a fence carries `language-*`; inline code has no class and stays
 * an ordinary `<code>`. `<pre>` unwraps to nothing because `Code` renders
 * its own. */
const components: Components = {
  pre: ({ children }) => <>{children}</>,
  code: ({ className, children }) => {
    const lang = /language-(\w+)/.exec(className ?? "")?.[1];
    if (lang === undefined) return <code>{children}</code>;
    return <Code text={String(children).replace(/\n$/, "")} lang={lang} />;
  },
};

export function Markdown({ children }: { children: string }) {
  return (
    <div className="prose">
      <ReactMarkdown remarkPlugins={[remarkGfm]} components={components}>
        {children}
      </ReactMarkdown>
    </div>
  );
}
