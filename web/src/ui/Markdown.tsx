// Prose rendering (docs/web-client.md). User turns, assistant text,
// thinking, and consolidation-mark notes are markdown and render as such;
// tool output is not prose and does not come through here — the cordon owns
// its representation. Thinking's markdown treatment carries a named reversal
// trigger (docs/web-client.md): a model whose thinking is line-structured
// rather than paragraph-separated would merge lines here, and that sighting
// flips thinking back to preformatted.
//
// The renderer produces a React element tree, never an HTML string: the
// text is model-authored, and a page holding a session credential must
// never turn untrusted content into markup (docs/web-client.md).
// Two properties of react-markdown carry that, and both are defaults rather
// than configuration — raw HTML is escaped unless `rehype-raw` is added, and
// `defaultUrlTransform` passes only http/https/irc/ircs/mailto/xmpp, so a
// `javascript:` or `data:` link in a model's output does not survive into an
// href. Neither should be switched off without reading that section.

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
