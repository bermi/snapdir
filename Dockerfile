FROM bermi/dirfest:0.1.1

COPY ./snapdir /bin/snapdir
RUN chmod +x /bin/snapdir && snapdir test

COPY ./snapdir-file-adapter /bin/snapdir-file-adapter
RUN chmod +x /bin/snapdir-file-adapter && snapdir-file-adapter test

ENTRYPOINT [ "/bin/snapdir" ]
