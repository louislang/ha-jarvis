ARG BUILD_FROM
FROM $BUILD_FROM
RUN apk add --no-cache nginx
COPY nginx.conf /etc/nginx/nginx.conf
COPY down.html /www/down.html
COPY run.sh /run.sh
CMD ["/run.sh"]
