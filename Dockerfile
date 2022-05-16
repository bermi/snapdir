FROM alpine:3.15

RUN apk add \
  --repository=http://dl-cdn.alpinelinux.org/alpine/edge/testing \
  --no-cache bash b3sum wget && \
  wget -p https://github.com/bermi/dirfest/releases/download/v0.1.0/dirfest -O dirfest && \
  chmod +x dirfest && \
  echo "33b378eb8d4de756a029771a3d5bb96e75ced4e79e6bcbb93ae6a4302f8b7eb2  dirfest" | b3sum -c && \
  mv dirfest /usr/local/bin/ && \
  apk del wget

COPY ./snapdir /bin/snapdir
RUN chmod +x /bin/snapdir

RUN snapdir test

ENTRYPOINT [ "/bin/snapdir" ]
