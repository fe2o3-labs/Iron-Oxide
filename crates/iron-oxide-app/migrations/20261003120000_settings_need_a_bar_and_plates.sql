-- Settings need a bar heavier than zero and at least one plate size (#34). The server refuses
-- anything else since #98; before, an empty inventory and a 0 kg bar were accepted, and the column
-- default was an empty inventory.
--
-- Stored rows are brought in line first (decision log #41: tightening a rule ships a migration
-- that fixes stored data), with the same defaults as the app (`Settings::defaults()`): a 20 kg
-- bar and the domain's default kg plate set (`PlateInventory::default_for(Kg)`). Then CHECK
-- constraints keep it that way.

UPDATE user_settings
   SET bar_weight_ng = 20000000000000, updated_at = now()
 WHERE bar_weight_ng <= 0;

UPDATE user_settings
   SET plate_inventory = '[{"plate": 25.0, "pairs": 4}, {"plate": 20.0, "pairs": 2}, {"plate": 15.0, "pairs": 1}, {"plate": 10.0, "pairs": 1}, {"plate": 5.0, "pairs": 1}, {"plate": 2.5, "pairs": 1}, {"plate": 1.25, "pairs": 1}]', updated_at = now()
 WHERE plate_inventory = '[]'::jsonb;

ALTER TABLE user_settings
    ALTER COLUMN plate_inventory SET DEFAULT '[{"plate": 25.0, "pairs": 4}, {"plate": 20.0, "pairs": 2}, {"plate": 15.0, "pairs": 1}, {"plate": 10.0, "pairs": 1}, {"plate": 5.0, "pairs": 1}, {"plate": 2.5, "pairs": 1}, {"plate": 1.25, "pairs": 1}]';

ALTER TABLE user_settings
    ADD CONSTRAINT user_settings_bar_weighs_something CHECK (bar_weight_ng > 0),
    ADD CONSTRAINT user_settings_has_plates CHECK (plate_inventory <> '[]'::jsonb);
