# Read-only search storage boundaries

`string-search` accepts Unicode strings, unibyte strings, and mixed inputs.
Unicode results/START count Unicode scalar characters; unibyte haystacks use
byte indices. ASCII patterns cross storage kinds without changing bytes.
Non-ASCII unibyte characters are not UTF8 or Latin1-decoded to match Unicode
scalars: the mixed-storage high-byte cases return nil, qualified against GNU.
The byte matcher uses prefix fallback and never decodes or copies the haystack.

`re-search-backward` additionally accepts ASCII unibyte regexp patterns against
the Unicode buffer model. Search-failed payloads retain the original pattern's
storage. Non-ASCII unibyte regexps remain explicitly unsupported; they are not
silently interpreted as UTF8. This does not change general string unboxing or
claim that every string primitive supports both storage kinds.
