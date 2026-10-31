export function isHandoff(
  content: string,
  handle: string | null | undefined,
): boolean {
  if (handle == null) return false;
  return (
    content.startsWith(`backgrounded as ${handle}.`) ||
    (content.startsWith("this command was still running after ") &&
      content.includes(
        `the background on its own as ${handle}. nothing was killed`,
      ))
  );
}
