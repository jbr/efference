export function isHandoff(
  content: string,
  handle: string | undefined,
): boolean {
  if (handle === undefined) return false;
  return (
    content.startsWith(`backgrounded as ${handle}.`) ||
    (content.startsWith("this command was still running after ") &&
      content.includes(
        `the background on its own as ${handle}. nothing was killed`,
      ))
  );
}
