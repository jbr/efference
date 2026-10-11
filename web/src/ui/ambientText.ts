/** Display-only terminal formatting cleanup (docs/web-client.md). */
export function ambientText(text: string): string {
  // CSI formatting and OSC links/titles are terminal instructions, not prose.
  return text.replace(
    // eslint-disable-next-line no-control-regex -- Terminal sequences contain literal control bytes.
    /(?:\x1b\[|\x9b)[0-?]*[ -/]*[@-~]|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)/g,
    "",
  );
}
