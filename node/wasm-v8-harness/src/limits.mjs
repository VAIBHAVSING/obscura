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
export const MAX_PLATFORM_PBKDF2_WORK_UNITS = 1_000_000;
export const MAX_RENDER_URL_BYTES = 64 * 1024;
export const MAX_RENDER_RESOURCE_BYTES = 16 * 1024 * 1024;
export const MAX_RENDER_RESOURCE_REQUESTS_PER_PAGE = 32;
export const RENDER_IMAGE_REQUEST_PROFILES = Object.freeze([
  "no-cors-include",
  "cors-same-origin",
  "cors-include",
]);
export const MAX_SCREENSHOT_DIMENSION = 32_768;
export const MAX_SCREENSHOT_PIXELS = 16_777_216;
export const MAX_SCREENSHOT_PNG_BYTES = 128 * 1024 * 1024;
export const PNG_HEADER_SIGNATURE = Object.freeze([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);

export function requireBoundedString(value, maximum, label) {
  if (typeof value !== "string") throw new TypeError(`${label} must be a string`);
  if (Buffer.byteLength(value, "utf8") > maximum) {
    throw new RangeError(`${label} exceeds the ${maximum}-byte ABI limit`);
  }
  return value;
}

export function requireBoundedBytes(value, maximum, label) {
  if (!(value instanceof Uint8Array) && !Buffer.isBuffer(value) && !ArrayBuffer.isView(value)) {
    throw new TypeError(`${label} must be a Uint8Array or Buffer`);
  }
  if (value.byteLength > maximum) {
    throw new RangeError(`${label} exceeds the ${maximum}-byte ABI limit`);
  }
  return new Uint8Array(value.buffer.slice(value.byteOffset, value.byteOffset + value.byteLength));
}

export function requireValidPngBytes(bytes, label = "screenshot PNG") {
  if (!(bytes instanceof Uint8Array) && !Buffer.isBuffer(bytes) && !ArrayBuffer.isView(bytes)) {
    throw new TypeError(`${label} must return a Uint8Array or Buffer`);
  }
  if (bytes.byteLength < PNG_HEADER_SIGNATURE.length) {
    throw new TypeError(`${label} must contain at least 8 bytes for PNG signature`);
  }
  if (bytes.byteLength > MAX_SCREENSHOT_PNG_BYTES) {
    throw new RangeError(`${label} exceeds the ${MAX_SCREENSHOT_PNG_BYTES}-byte ABI limit`);
  }
  const view = new Uint8Array(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  for (let i = 0; i < PNG_HEADER_SIGNATURE.length; i++) {
    if (view[i] !== PNG_HEADER_SIGNATURE[i]) {
      throw new TypeError(`${label} does not start with a valid 8-byte PNG signature`);
    }
  }
  return new Uint8Array(bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength));
}

export function requireRenderImageRequestProfile(value) {
  if (typeof value !== "string" || !RENDER_IMAGE_REQUEST_PROFILES.includes(value)) {
    throw new TypeError(
      `render image request profile must be one of ${RENDER_IMAGE_REQUEST_PROFILES.join(", ")}`,
    );
  }
  return value;
}
