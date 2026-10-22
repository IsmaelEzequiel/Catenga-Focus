# Build context is the repo root: docker build -f server/Dockerfile .
FROM node:22-slim AS build
RUN corepack enable && corepack prepare pnpm@8.15.4 --activate
WORKDIR /repo
COPY pnpm-workspace.yaml pnpm-lock.yaml package.json ./
COPY server/package.json server/
# The workspace also lists apps/*; --filter limits the install to the server.
RUN pnpm install --frozen-lockfile --filter @catenga/server...
COPY server server
RUN pnpm --filter @catenga/server build \
 && pnpm --filter @catenga/server deploy --prod /out \
 && cp -r server/dist /out/dist

FROM node:22-slim
WORKDIR /app
RUN chown node:node /app
COPY --from=build --chown=node:node /out .
USER node
ENV PORT=8080 NODE_ENV=production
EXPOSE 8080
CMD ["node", "dist/index.js"]
