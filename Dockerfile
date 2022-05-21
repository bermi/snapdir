FROM alpine:3.15

RUN apk add \
  --repository=http://dl-cdn.alpinelinux.org/alpine/edge/testing \
  --no-cache bash b3sum wget

COPY ./snapdir* /bin/
RUN chmod +x /bin/snapdir* && snapdir-test

RUN apk del wget && rm -rf /var/cache/apk/*

ENTRYPOINT [ "/bin/snapdir" ]
