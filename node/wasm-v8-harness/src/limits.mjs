import { Buffer } from "node:buffer";

export const MAX_HTML_INPUT_BYTES = 8 * 1024 * 1024;
export const MAX_SELECTOR_BYTES = 64 * 1024;
export const MAX_RETURNED_STRING_BYTES = 4 * 1024 * 1024;
export const MAX_DOM_COMMAND_BYTES = 64;
export const MAX_DOM_ARGUMENT_BYTES = 8 * 1024 * 1024;
export const MAX_DOM_BATCH_BYTES = 8 * 1024 * 1024;
export const MAX_DOM_BATCH_OPERATIONS = 1_024;
export const MAX_DOCUMENT_METADATA_BYTES = 64 * 1024;

export function requireBoundedString(value, maximum, label) {
  if (typeof value !== "string") throw new TypeError(`${label} must be a string`);
  if (Buffer.byteLength(value, "utf8") > maximum) {
    throw new RangeError(`${label} exceeds the ${maximum}-byte ABI limit`);
  }
  return value;
}
