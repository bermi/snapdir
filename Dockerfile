FROM alpine:3.16

RUN apk add \
  --repository=http://dl-cdn.alpinelinux.org/alpine/edge/testing \
  --no-cache bash b3sum wget

COPY ./snapdir* /bin/
RUN chmod +x /bin/snapdir*
RUN snapdir-test && rm -rf /tmp/snapdir*

RUN apk del wget && rm -rf /var/cache/apk/*

LABEL \
  description="Snapdir. Authenticated directory snapshots" \
  canonical_url="https://github.com/bermi/snapdir" \
  license="MIT" \
  maintainer="bermi" \
  authors="bermi"

ENTRYPOINT [ "/bin/snapdir" ]
