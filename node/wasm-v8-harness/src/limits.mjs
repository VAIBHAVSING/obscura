import { Buffer } from "node:buffer";

export const MAX_HTML_INPUT_BYTES = 8 * 1024 * 1024;
export const MAX_SELECTOR_BYTES = 64 * 1024;
export const MAX_RETURNED_STRING_BYTES = 4 * 1024 * 1024;
export const MAX_DOM_COMMAND_BYTES = 64;
export const MAX_DOM_ARGUMENT_BYTES = 8 * 1024 * 1024;
export const MAX_DOM_BATCH_BYTES = 8 * 1024 * 1024;
export const MAX_DOM_BATCH_OPERATIONS = 1_024;
export const MAX_DOCUMENT_METADATA_BYTES = 64 * 1024;
export const MAX_PLATFORM_COMMAND_BYTES = 64;
// Platform requests carry binary arguments as base64, so leave room for the
// 4/3 expansion above the ordinary 8 MiB page-input ceiling.
export const MAX_PLATFORM_REQUEST_BYTES = 12 * 1024 * 1024;
export const MAX_PLATFORM_RESPONSE_BYTES = 12 * 1024 * 1024;
export const MAX_PLATFORM_BINARY_BYTES = 8 * 1024 * 1024;
export const MAX_PLATFORM_RANDOM_BYTES = 65_536;
export const MAX_PLATFORM_KDF_OUTPUT_BYTES = 1024 * 1024;
export const MAX_PLATFORM_PBKDF2_ITERATIONS = 1_000_000;

export function requireBoundedString(value, maximum, label) {
  if (typeof value !== "string") throw new TypeError(`${label} must be a string`);
  if (Buffer.byteLength(value, "utf8") > maximum) {
    throw new RangeError(`${label} exceeds the ${maximum}-byte ABI limit`);
  }
  return value;
}
