export interface LaunchOptions {
  modulePath?: string;
  host?: string;
  port?: number;
  bootstrapPath?: string;
  readyTimeoutMs?: number;
  requestTimeoutMs?: number;
  evaluateTimeoutMs?: number;
  taskTimeoutMs?: number;
  allowPrivateNetwork?: boolean;
  workerOptions?: Record<string, unknown>;
}

export interface ObscuraBrowser {
  httpEndpoint(): string;
  wsEndpoint(): string;
  processInfo(): { pid: number; host: string; port: number | null; native: false; wasm: true };
  close(): Promise<void>;
}

export interface CdpClient {
  ready(): Promise<this>;
  command<T = unknown>(method: string, params?: Record<string, unknown>, sessionId?: string): Promise<T>;
  onEvent(listener: (event: Record<string, unknown>) => void): () => boolean;
  close(): Promise<void>;
}

export function launch(options?: LaunchOptions): Promise<ObscuraBrowser>;
export function connect(endpoint: string, options?: Record<string, unknown>): Promise<CdpClient>;
export function version(): string;
