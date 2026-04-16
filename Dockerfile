# For better debugging, you can use Debian instead
# FROM node:22-trixie-slim
# 
# RUN apt-get update \
# 	&& apt-get install -y --no-install-recommends ffmpeg \
# 	&& rm -rf /var/lib/apt/lists/*

FROM node:22-alpine

RUN apk add --no-cache ffmpeg

WORKDIR /app

COPY package.json package-lock.json ./
RUN npm ci --omit=dev

COPY src ./src
COPY public ./public

EXPOSE 3030

ENV NODE_ENV=production
ENV PORT=3030

CMD ["npm", "start"]
