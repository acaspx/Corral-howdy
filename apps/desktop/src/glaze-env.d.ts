// Glaze's components read the build mode to warn in development; Vite replaces it at build time.
declare const process: { env: { NODE_ENV?: string } };
