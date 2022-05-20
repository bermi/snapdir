FROM alpine:3.15

RUN apk add \
  --repository=http://dl-cdn.alpinelinux.org/alpine/edge/testing \
  --no-cache bash b3sum

COPY ./snapdir* /bin/
RUN chmod +x /bin/snapdir* && snapdir-test

ENTRYPOINT [ "/bin/snapdir" ]
