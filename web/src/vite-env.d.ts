/// <reference types="vite/client" />

import type React from "react";

declare module "react" {
  namespace JSX {
    interface IntrinsicElements {
      "relative-time": React.DetailedHTMLProps<
        React.HTMLAttributes<HTMLElement> & {
          datetime?: string;
          tense?: string;
          format?: string;
          class?: string;
        },
        HTMLElement
      >;
    }
  }
}

declare global {
  namespace JSX {
    interface IntrinsicElements {
      "relative-time": React.DetailedHTMLProps<
        React.HTMLAttributes<HTMLElement> & {
          datetime?: string;
          tense?: string;
          format?: string;
          class?: string;
        },
        HTMLElement
      >;
    }
  }
}
