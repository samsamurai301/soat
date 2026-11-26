-- B7: an image's pixel size and the smaller widths made of it.
ALTER TABLE files
    ADD COLUMN width INTEGER,
    ADD COLUMN height INTEGER,
    ADD COLUMN sizes INTEGER[] NOT NULL DEFAULT '{}';
