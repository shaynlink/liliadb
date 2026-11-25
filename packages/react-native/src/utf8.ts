// UTF-8 decoding: React Native does not polyfill `TextDecoder`, and not every Hermes release
// has it (`TextEncoder` it has).

/** `bytes` as text; each byte of a malformed sequence reads as U+FFFD. */
export function decodeUtf8(bytes: Uint8Array): string {
  const codes: number[] = [];
  let text = "";
  for (let i = 0; i < bytes.length;) {
    const lead = bytes[i]!;
    const length =
      lead < 0x80
        ? 1
        : lead >= 0xf0 && lead < 0xf5
          ? 4
          : lead >= 0xe0
            ? 3
            : lead >= 0xc2 && lead < 0xe0
              ? 2
              : 0;
    let code =
      length === 1 ? lead : length === 2 ? lead & 0x1f : length === 3 ? lead & 0x0f : lead & 0x07;
    let valid = length > 0 && i + length <= bytes.length;
    for (let k = 1; valid && k < length; k++) {
      const next = bytes[i + k]!;
      valid = (next & 0xc0) === 0x80;
      code = (code << 6) | (next & 0x3f);
    }
    // Overlong encodings, surrogates and code points past U+10FFFF are malformed too.
    const minimum = [0, 0, 0x80, 0x800, 0x10000][length]!;
    if (!valid || code < minimum || (code >= 0xd800 && code <= 0xdfff) || code > 0x10ffff) {
      codes.push(0xfffd);
      i += 1;
    } else {
      if (code > 0xffff) {
        code -= 0x10000;
        codes.push(0xd800 + (code >> 10), 0xdc00 + (code & 0x3ff));
      } else {
        codes.push(code);
      }
      i += length;
    }
    if (codes.length >= 4096) {
      text += String.fromCharCode(...codes);
      codes.length = 0;
    }
  }
  return text + String.fromCharCode(...codes);
}
