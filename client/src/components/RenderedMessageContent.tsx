import { type ReactNode } from "react";
import type { AttachmentInfo } from "../lib/types";
import type { BookItem } from "../lib/book/types";
import { lookupShorthand, renderUnicodeEmoji } from "../lib/unicodeEmoji";
import InlineBookEmoji from "./InlineBookEmoji";

interface Token {
  name: string; // text inside the colons, lowercased
  index: number; // start position in source text (the opening colon)
  length: number; // total length including both colons
}

const TOKEN_REGEX = /:([a-z0-9_+\-]+):/gi;

export function parseColonTokens(text: string): Token[] {
  const tokens: Token[] = [];
  let m: RegExpExecArray | null;
  TOKEN_REGEX.lastIndex = 0;
  while ((m = TOKEN_REGEX.exec(text)) !== null) {
    tokens.push({
      name: m[1].toLowerCase(),
      index: m.index,
      length: m[0].length,
    });
  }
  return tokens;
}

interface Props {
  text: string;
  attachments: AttachmentInfo[];
  bookIndex: BookItem[];
  serverId: string;
  /** Renders attachments not consumed as inline emoji. Pass through unchanged. */
  renderRemainingAttachments: (remaining: AttachmentInfo[]) => ReactNode;
  /** Optional: render plain-text segments through a transform (e.g. mention highlighting).
   *  Default: identity. */
  renderTextSegment?: (text: string) => ReactNode;
}

export default function RenderedMessageContent({
  text,
  attachments,
  bookIndex,
  serverId,
  renderRemainingAttachments,
  renderTextSegment,
}: Props) {
  const renderText = renderTextSegment ?? ((t: string) => t);
  const tokens = parseColonTokens(text);
  const usedAttachmentIds = new Set<number>();
  const out: ReactNode[] = [];
  let cursor = 0;
  let key = 0;

  for (const tok of tokens) {
    // Emit the text segment before this token.
    if (tok.index > cursor) {
      out.push(<span key={key++}>{renderText(text.slice(cursor, tok.index))}</span>);
    }
    cursor = tok.index + tok.length;

    // Try book item match first (book wins on collision).
    const bookMatch = bookIndex.find((b) => b.name === tok.name);
    if (bookMatch) {
      const expectedFilename = `${bookMatch.name}.${bookMatch.ext}`;
      const att = attachments.find(
        (a) => a.name === expectedFilename && !usedAttachmentIds.has(a.file_id),
      );
      if (att) {
        usedAttachmentIds.add(att.file_id);
        out.push(
          <InlineBookEmoji
            key={key++}
            attachment={att}
            serverId={serverId}
            altText={`:${tok.name}:`}
          />,
        );
        continue;
      }
    }

    // Try Unicode shorthand.
    const codepoint = lookupShorthand(tok.name);
    if (codepoint) {
      out.push(<span key={key++}>{renderUnicodeEmoji(codepoint)}</span>);
      continue;
    }

    // No match — render the token as literal text.
    out.push(<span key={key++}>{text.slice(tok.index, tok.index + tok.length)}</span>);
  }

  // Trailing text after the last token.
  if (cursor < text.length) {
    out.push(<span key={key++}>{renderText(text.slice(cursor))}</span>);
  }

  // Attachments not consumed as inline emoji render normally below the text.
  const remaining = attachments.filter((a) => !usedAttachmentIds.has(a.file_id));

  return (
    <>
      <span className="message-content">{out}</span>
      {renderRemainingAttachments(remaining)}
    </>
  );
}
