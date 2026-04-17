FROM node:22-bullseye-slim

WORKDIR /app

RUN apt-get update \
	&& apt-get install -y --no-install-recommends ffmpeg \
	&& rm -rf /var/lib/apt/lists/*

COPY package.json package-lock.json ./
RUN npm ci --omit=dev

COPY src ./src
COPY public ./public

EXPOSE 3030

ENV NODE_ENV=production
ENV PORT=3030

CMD ["npm", "start"]
