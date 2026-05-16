/// <reference types="vite/client" />

// Vite-specific `?url` import suffix returns the asset URL as a string.
declare module "*?url" {
  const url: string;
  export default url;
}
