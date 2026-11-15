import { closeHistory } from "@tiptap/pm/history";
import { NodeViewWrapper, type NodeViewProps } from "@tiptap/react";
import type { ReferenceDefinition } from "../api/types.ts";
import { ReferenceChip } from "./ComposerReferences.tsx";

/** A reference's interactive chip. The node view has no store of its own:
 * removal is a document edit, and the store receives it through `onUpdate`. */
export function ComposerChip({
  node,
  editor,
  updateAttributes,
  deleteNode,
}: NodeViewProps) {
  const definition = node.attrs.definition as ReferenceDefinition;
  const target = definition.target;
  return (
    <NodeViewWrapper
      as="span"
      className="inline-reference-node"
      contentEditable={false}
    >
      <ReferenceChip
        label={definition.label}
        detail={
          target.type === "filesystem"
            ? target.path
            : target.type === "agent"
              ? `Exact session: ${target.session}`
              : target.name
        }
        kind={target.type === "filesystem" ? target.target : target.type}
        write={target.type === "filesystem" && target.access.write}
        onWriteChange={(write) => {
          if (target.type === "filesystem") {
            // A toggle is its own undo step, not merged with nearby typing.
            editor.view.dispatch(closeHistory(editor.state.tr));
            updateAttributes({
              definition: {
                ...definition,
                target: { ...target, access: { read: true, write } },
              },
            });
          }
        }}
        onRemove={() => {
          // Removing a chip detaches its occurrence; it never deletes stored
          // bytes. A recalled attachment's upload belongs to the sent turn,
          // and undo must be able to restore the chip (docs/web-client.md).
          editor.view.dispatch(closeHistory(editor.state.tr));
          deleteNode();
        }}
      />
    </NodeViewWrapper>
  );
}
