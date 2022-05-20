FROM alpine:3.15

RUN apk add \
  --repository=http://dl-cdn.alpinelinux.org/alpine/edge/testing \
  --no-cache bash b3sum

COPY ./snapdir-manifest /bin/snapdir-manifest
RUN chmod +x /bin/snapdir-manifest && snapdir-manifest test

COPY ./snapdir /bin/snapdir
RUN chmod +x /bin/snapdir && snapdir test

COPY ./snapdir-file-adapter /bin/snapdir-file-adapter
RUN chmod +x /bin/snapdir-file-adapter && snapdir-file-adapter test

ENTRYPOINT [ "/bin/snapdir" ]
