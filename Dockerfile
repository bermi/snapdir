FROM bermi/dirfest:0.1.1

COPY ./snapdir /bin/snapdir
RUN chmod +x /bin/snapdir

RUN snapdir test

ENTRYPOINT [ "/bin/snapdir" ]
